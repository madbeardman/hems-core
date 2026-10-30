//! Local history. SQLite work is serialized off the async executor.
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone)]
pub struct History {
    db: Arc<Mutex<Connection>>,
    slots: Arc<tokio::sync::Semaphore>,
    pub config: HistoryConfig,
}
#[derive(Clone)]
pub struct HistoryConfig {
    pub path: String,
    pub interval: u64,
    pub days: i64,
}
impl HistoryConfig {
    pub fn from_env() -> Result<Self, Error> {
        fn number(name: &str, default: i64, min: i64, max: i64) -> Result<i64, Error> {
            let n = std::env::var(name).map_or(Ok(default), |s| s.parse::<i64>())?;
            if !(min..=max).contains(&n) {
                return Err(format!("{name} outside {min}..={max}").into());
            }
            Ok(n)
        }
        Ok(Self {
            path: std::env::var("HEMS_DB_PATH").unwrap_or_else(|_| "data/history.sqlite3".into()),
            interval: number("HEMS_HISTORY_INTERVAL_SECONDS", 60, 60, 3600)? as u64,
            days: number("HEMS_HISTORY_RETENTION_DAYS", 30, 1, 3650)?,
        })
    }
}
impl History {
    pub fn open(config: HistoryConfig) -> Result<Self, Error> {
        if let Some(parent) = std::path::Path::new(&config.path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut db = Connection::open(&config.path)?;
        db.busy_timeout(Duration::from_secs(2))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )?;
        migrate(&mut db)?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
            slots: Arc::new(tokio::sync::Semaphore::new(4)),
            config,
        })
    }
    pub(crate) async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "history busy")?;
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut connection = db.lock().map_err(|_| "history lock poisoned")?;
            f(&mut connection)
        })
        .await?
    }
    pub async fn record(
        &self,
        kind: &'static str,
        payload: Value,
        fetched: DateTime<Utc>,
    ) -> Result<(), Error> {
        let interval = self.config.interval as i64;
        self.run(move |db| record(db, kind, &payload, fetched, interval))
            .await
    }
    async fn prune(&self) -> Result<(), Error> {
        let cutoff = (Utc::now() - chrono::Duration::days(self.config.days)).timestamp();
        // Bounded deletion batches avoid long write locks on SD cards. Old data is
        // removed gradually if retention is shortened substantially.
        self.run(move |db| { let tx = db.transaction()?;
            for (table, column) in [("samples", "sampled_at"), ("snapshots", "first_seen"), ("prices", "end_at")] {
                tx.execute(&format!("DELETE FROM {table} WHERE rowid IN (SELECT rowid FROM {table} WHERE {column} < ? LIMIT 5000)"), [cutoff])?;
            } tx.commit()?; Ok(()) }).await
    }
}
fn migrate(db: &mut Connection) -> Result<(), Error> {
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > 1 {
        return Err("History database is newer than this binary".into());
    }
    if version == 0 {
        let tx = db.transaction()?;
        tx.execute_batch("CREATE TABLE samples (
            sampled_at INTEGER NOT NULL, metric TEXT NOT NULL, fetched_at TEXT NOT NULL,
            observed_at TEXT, value REAL, unit TEXT, status TEXT NOT NULL, provenance TEXT NOT NULL,
            PRIMARY KEY(metric,sampled_at));
            CREATE INDEX samples_time ON samples(sampled_at);
            CREATE TABLE snapshots (
            id INTEGER PRIMARY KEY, kind TEXT NOT NULL, source TEXT NOT NULL, target_day TEXT NOT NULL,
            first_seen INTEGER NOT NULL, fetched_at TEXT NOT NULL, observed_at TEXT, issued_at TEXT,
            status TEXT NOT NULL, identity TEXT NOT NULL, payload TEXT NOT NULL,
            UNIQUE(kind,source,target_day,first_seen,identity));
            CREATE INDEX snapshots_latest ON snapshots(kind,source,target_day,id);
            CREATE INDEX snapshots_query ON snapshots(kind,target_day,first_seen,id);
            CREATE INDEX snapshots_retention ON snapshots(first_seen);
            CREATE TABLE prices (
            id INTEGER PRIMARY KEY, tariff TEXT NOT NULL, start_at INTEGER NOT NULL, end_at INTEGER NOT NULL,
            value REAL NOT NULL, first_seen INTEGER NOT NULL, provenance TEXT NOT NULL,
            UNIQUE(tariff,start_at,end_at,first_seen,value));
            CREATE INDEX prices_latest ON prices(tariff,start_at,end_at,id);
            CREATE INDEX prices_time ON prices(start_at,end_at);
            CREATE INDEX prices_retention ON prices(end_at);
            PRAGMA user_version=1;")?;
        tx.commit()?;
    }
    Ok(())
}
fn sample(
    db: &Connection,
    metric: &str,
    entry: &Value,
    fetched: DateTime<Utc>,
    interval: i64,
) -> Result<(), Error> {
    let status = entry["status"].as_str().unwrap_or("unavailable");
    let reading = &entry["reading"];
    let observed = reading["observed_at"]
        .as_str()
        .or(reading["observed_from"].as_str())
        .or(entry["source_timestamps"]["observed_at"].as_str());
    // Assumed-zero and derived estimates retain their explicit status. Only real
    // readings carry source timestamps; missing/invalid data is a gap, not zero.
    let value = if matches!(
        status,
        "valid"
            | "derived"
            | "estimated_with_assumed_zero"
            | "delayed_period"
            | "assumed_zero"
            | "fresh"
            | "older_report_accepted_via_group"
            | "future_within_tolerance"
    ) {
        reading["value"].as_f64()
    } else {
        None
    };
    db.execute(
        "INSERT OR IGNORE INTO samples VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            fetched.timestamp() / interval * interval,
            metric,
            entry["fetched_at"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| fetched.to_rfc3339()),
            observed,
            value,
            reading["unit"].as_str(),
            status,
            entry.to_string()
        ],
    )?;
    Ok(())
}
fn target_day(entry: &Value, fetched: DateTime<Utc>) -> String {
    // HA's today/tomorrow sensor refers to the London date of the source update,
    // not the date we happen to fetch it (important around midnight / outages).
    let source = entry["source_timestamps"]["last_updated"]
        .as_str()
        .and_then(|s| s.parse::<DateTime<Utc>>().ok())
        .unwrap_or(fetched);
    let date = source
        .with_timezone(&chrono_tz::Europe::London)
        .date_naive();
    (date + chrono::Duration::days(if entry["horizon"] == "tomorrow" { 1 } else { 0 })).to_string()
}
fn record(
    db: &mut Connection,
    kind: &str,
    payload: &Value,
    fetched: DateTime<Utc>,
    interval: i64,
) -> Result<(), Error> {
    let tx = db.transaction()?;
    match kind {
        "energy" | "costs" => {
            if let Some(entries) = payload.as_object() {
                for (metric, entry) in entries {
                    if entry.is_object() && entry.get("status").is_some() {
                        sample(&tx, metric, entry, fetched, interval)?;
                    }
                }
            }
        }
        "battery" => {
            for metric in ["soc_percent", "charge_power_w", "discharge_power_w"] {
                let mut reading = payload["freshness"]["sources"][metric].clone();
                if !reading.is_object() {
                    reading = json!({});
                }
                reading["value"] = payload[metric].clone();
                reading["unit"] = json!(if metric == "soc_percent" { "%" } else { "W" });
                let mut group = payload["freshness"].clone();
                if let Some(object) = group.as_object_mut() {
                    object.remove("sources");
                }
                let entry = json!({"status":reading["status"].as_str().or(payload["status"].as_str()).unwrap_or("unavailable"),"reading":reading,"freshness":group,"fetched_at":payload["freshness"]["fetched_at"]});
                sample(&tx, metric, &entry, fetched, interval)?;
            }
        }
        "forecasts" => {
            if let Some(entries) = payload["forecasts"].as_array() {
                for entry in entries {
                    let mut entry = entry.clone();
                    entry["target_day_basis"] = json!(
                        "Europe/London source last_updated date plus configured horizon; fallback fetch date when no source timestamp; provider target date unavailable"
                    );
                    entry["provider_issued_at"] = Value::Null;
                    let day = target_day(&entry, fetched);
                    snapshot(
                        &tx,
                        "forecasts",
                        &format!(
                            "{}:{}:{}",
                            entry["array"].as_str().unwrap_or("unknown"),
                            entry["horizon"].as_str().unwrap_or("unknown"),
                            entry["entity_id"].as_str().unwrap_or("")
                        ),
                        &day,
                        &entry,
                        fetched,
                    )?;
                }
            }
        }
        "prices" => {
            for feed in [payload, &payload["next_day"]] {
                if !feed.is_object() {
                    continue;
                }
                let mut feed = feed.clone();
                feed.as_object_mut().unwrap().remove("next_day");
                // Dynamic 'current' changes aren't schedule revisions.
                if let Some(data) = feed["data"].as_object_mut() {
                    data.remove("current");
                    data.remove("has_current_interval");
                }
                snapshot(
                    &tx,
                    "prices",
                    feed["entity_id"].as_str().unwrap_or("unconfigured"),
                    "",
                    &feed,
                    fetched,
                )?;
                if let Some(rates) = feed["data"]["intervals"].as_array() {
                    for rate in rates {
                        let start = rate["start"]
                            .as_str()
                            .and_then(|s| s.parse::<DateTime<Utc>>().ok());
                        let end = rate["end"]
                            .as_str()
                            .and_then(|s| s.parse::<DateTime<Utc>>().ok());
                        if let (Some(start), Some(end), Some(value), Some(tariff)) = (
                            start,
                            end,
                            rate["value_inc_vat"].as_f64(),
                            feed["data"]["tariff_code"].as_str(),
                        ) {
                            let latest: Option<f64> = tx.query_row("SELECT value FROM prices WHERE tariff=?1 AND start_at=?2 AND end_at=?3 ORDER BY id DESC LIMIT 1",params![tariff,start.timestamp(),end.timestamp()],|r|r.get(0)).optional()?;
                            if latest == Some(value) {
                                continue;
                            }
                            tx.execute("INSERT OR IGNORE INTO prices(tariff,start_at,end_at,value,first_seen,provenance) VALUES (?1,?2,?3,?4,?5,?6)", params![tariff,start.timestamp(),end.timestamp(),value,fetched.timestamp(),json!({"entity_id":feed["entity_id"],"last_updated":feed["last_updated"],"last_changed":feed["last_changed"],"last_reported":feed["last_reported"],"fetched_at":fetched,"status":feed["status"],"unit":"GBP/kWh","vat_included":true,"interval":rate}).to_string()])?;
                        }
                    }
                }
            }
        }
        _ => return Err("unknown history kind".into()),
    }
    tx.commit()?;
    Ok(())
}
fn snapshot(
    db: &Connection,
    kind: &str,
    source: &str,
    day: &str,
    entry: &Value,
    fetched: DateTime<Utc>,
) -> Result<(), Error> {
    // Full canonical JSON identity avoids hash collisions. Do not include fetch
    // time: identical cache responses and process restarts must not add revisions.
    let mut identity = entry.clone();
    identity.as_object_mut().unwrap().remove("fetched_at");
    identity.as_object_mut().unwrap().remove("last_reported");
    for key in ["source_timestamps", "reading"] {
        if let Some(object) = identity[key].as_object_mut() {
            object.remove("last_reported");
            object.remove("observed_at");
        }
    }
    // Reports of unchanged cached values are observations, not new forecast
    // revisions. The first snapshot retains the original report/fetch metadata.
    let identity = identity.to_string();
    let latest: Option<String> = db.query_row("SELECT identity FROM snapshots WHERE kind=?1 AND source=?2 AND target_day=?3 ORDER BY id DESC LIMIT 1",params![kind,source,day],|r|r.get(0)).optional()?;
    if latest.as_deref() == Some(identity.as_str()) {
        return Ok(());
    }
    db.execute("INSERT OR IGNORE INTO snapshots(kind,source,target_day,first_seen,fetched_at,observed_at,issued_at,status,identity,payload) VALUES (?1,?2,?3,?4,?5,?6,NULL,?7,?8,?9)",params![kind,source,day,fetched.timestamp(),entry["fetched_at"].as_str().map(str::to_owned).unwrap_or_else(|| fetched.to_rfc3339()),entry["source_timestamps"]["observed_at"].as_str().or(entry["last_reported"].as_str()).or(entry["last_updated"].as_str()),entry["status"].as_str().unwrap_or("unavailable"),identity,entry.to_string()])?;
    Ok(())
}

pub fn start(
    history: History,
    state: crate::api::AppState,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(history.config.interval));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut schedules = tokio::time::Instant::now() - Duration::from_secs(300);
        loop {
            tokio::select! { _ = stop.changed() => break, _ = tick.tick() => {} }
            let cycle = async {
                let (battery, energy) = tokio::join!(
                    crate::api::history_battery(state.clone()),
                    crate::api::history_energy(state.clone())
                );
                for (kind, payload) in [("battery", battery), ("energy", energy)] {
                    if history.record(kind, payload, Utc::now()).await.is_err() {
                        eprintln!("History telemetry write failed");
                    }
                }
                if schedules.elapsed() >= Duration::from_secs(300) {
                    let (prices, forecasts) = tokio::join!(
                        crate::api::history_prices(state.clone()),
                        crate::api::history_forecasts(state.clone())
                    );
                    for (kind, payload) in [("prices", prices), ("forecasts", forecasts)] {
                        if history.record(kind, payload, Utc::now()).await.is_err() {
                            eprintln!("History schedule write failed");
                        }
                    }
                    schedules = tokio::time::Instant::now();
                    if history.prune().await.is_err() {
                        eprintln!("History retention failed");
                    }
                }
            };
            tokio::select! { _ = stop.changed() => break, result = tokio::time::timeout(Duration::from_secs(180),cycle) => { if result.is_err() { eprintln!("History collection timed out"); } } }
        }
    })
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {
    from: Option<i64>,
    to: Option<i64>,
    metric: Option<String>,
    kind: Option<String>,
    target_day: Option<String>,
    as_of: Option<i64>,
    limit: Option<usize>,
}
impl HistoryQuery {
    fn bounds(&self) -> Result<(i64, i64, usize), &'static str> {
        let to = self.to.unwrap_or_else(|| Utc::now().timestamp());
        let from = self
            .from
            .or_else(|| to.checked_sub(86400))
            .ok_or("Invalid time range")?;
        let limit = self.limit.unwrap_or(1000);
        if from < 0 || to <= from || to - from > 31 * 86400 || !(1..=2000).contains(&limit) {
            return Err("Use a positive range of at most 31 days and limit 1..2000");
        }
        Ok((from, to, limit))
    }
}
type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;
fn bad(message: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({"error":message})))
}
fn unavailable(_: Error) -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error":"History unavailable"})),
    )
}
async fn samples(State(h): State<History>, Query(q): Query<HistoryQuery>) -> ApiResult {
    let (from, to, limit) = q.bounds().map_err(bad)?;
    let metric = q.metric.ok_or_else(|| bad("metric is required"))?;
    let rows = h.run(move |db| {
        let mut stmt = db.prepare("SELECT sampled_at,fetched_at,observed_at,value,unit,status,provenance FROM samples WHERE metric=?1 AND sampled_at>=?2 AND sampled_at<?3 ORDER BY sampled_at LIMIT ?4")?;
        let rows = stmt.query_map(params![metric,from,to,(limit+1) as i64], |r| Ok(json!({"sampled_at":r.get::<_,i64>(0)?,"fetched_at":r.get::<_,String>(1)?,"observed_at":r.get::<_,Option<String>>(2)?,"value":r.get::<_,Option<f64>>(3)?,"unit":r.get::<_,Option<String>>(4)?,"status":r.get::<_,String>(5)?,"provenance":serde_json::from_str::<Value>(&r.get::<_,String>(6)?).unwrap_or(Value::Null)})))?.collect::<Result<Vec<_>,_>>()?;
        Ok(rows)
    }).await.map_err(unavailable)?;
    Ok(Json(page(rows, limit)))
}
fn page(mut rows: Vec<Value>, limit: usize) -> Value {
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    json!({"rows":rows,"truncated":truncated,"range":"from inclusive, to exclusive; Unix UTC seconds"})
}
async fn snapshots(State(h): State<History>, Query(q): Query<HistoryQuery>) -> ApiResult {
    let (from, to, limit) = q.bounds().map_err(bad)?;
    let kind = q.kind.unwrap_or_else(|| "forecasts".into());
    if !matches!(kind.as_str(), "forecasts" | "prices") {
        return Err(bad("kind must be forecasts or prices"));
    }
    let as_of = q.as_of.unwrap_or(to);
    let rows = h.run(move |db| {
        let mut stmt = db.prepare("SELECT id,source,target_day,first_seen,fetched_at,observed_at,issued_at,status,payload FROM snapshots WHERE kind=?1 AND first_seen>=?2 AND first_seen<?3 AND first_seen<=?4 AND (?5 IS NULL OR target_day=?5) ORDER BY first_seen,id LIMIT ?6")?;
        let rows = stmt.query_map(params![kind,from,to,as_of,q.target_day,(limit+1) as i64], |r| Ok(json!({"id":r.get::<_,i64>(0)?,"source":r.get::<_,String>(1)?,"target_day":r.get::<_,String>(2)?,"first_seen":r.get::<_,i64>(3)?,"fetched_at":r.get::<_,String>(4)?,"observed_at":r.get::<_,Option<String>>(5)?,"provider_issued_at":r.get::<_,Option<String>>(6)?,"status":r.get::<_,String>(7)?,"payload":serde_json::from_str::<Value>(&r.get::<_,String>(8)?).unwrap_or(Value::Null)})))?.collect::<Result<Vec<_>,_>>()?; Ok(rows)
    }).await.map_err(unavailable)?;
    Ok(Json(page(rows, limit)))
}
async fn prices(State(h): State<History>, Query(q): Query<HistoryQuery>) -> ApiResult {
    let (from, to, limit) = q.bounds().map_err(bad)?;
    let as_of = q.as_of.unwrap_or_else(|| Utc::now().timestamp());
    let rows = h.run(move |db| {
        let mut stmt = db.prepare("SELECT tariff,start_at,end_at,value,first_seen,provenance,id FROM prices WHERE start_at>=?1 AND start_at<?2 AND first_seen<=?3 ORDER BY start_at,first_seen,id LIMIT ?4")?;
        let rows = stmt.query_map(params![from,to,as_of,(limit+1) as i64], |r| Ok(json!({"revision_id":r.get::<_,i64>(6)?,"tariff":r.get::<_,String>(0)?,"start":r.get::<_,i64>(1)?,"end":r.get::<_,i64>(2)?,"value_inc_vat":r.get::<_,f64>(3)?,"first_seen":r.get::<_,i64>(4)?,"provenance":serde_json::from_str::<Value>(&r.get::<_,String>(5)?).unwrap_or(Value::Null)})))?.collect::<Result<Vec<_>,_>>()?; Ok(rows)
    }).await.map_err(unavailable)?;
    Ok(Json(page(rows, limit)))
}
async fn status(State(h): State<History>) -> ApiResult {
    let interval = h.config.interval;
    let days = h.config.days;
    let result = h.run(move |db| {
        let mut counts = json!({});
        for table in ["samples","snapshots","prices"] { let count: i64 = db.query_row(&format!("SELECT COUNT(*) FROM {table}"),[],|r|r.get(0))?; counts[table] = json!(count); }
        let latest: Option<String> = db.query_row("SELECT MAX(fetched_at) FROM samples",[],|r|r.get(0))?;
        Ok(json!({"schema_version":1,"sample_interval_seconds":interval,"retention_days":days,"counts":counts,"last_sample_fetch":latest}))
    }).await.map_err(unavailable)?;
    Ok(Json(result))
}
pub fn router(history: History) -> Router {
    Router::new()
        .route("/samples", get(samples))
        .route("/snapshots", get(snapshots))
        .route("/prices", get(prices))
        .route("/status", get(status))
        .with_state(history)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(path: String) -> HistoryConfig {
        HistoryConfig {
            path,
            interval: 60,
            days: 30,
        }
    }
    fn time(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }
    fn forecast_payload(observed: &str, status: &str, value: Value) -> Value {
        json!({"forecasts":[{"array":"fit","orientation":"SW","horizon":"tomorrow","entity_id":"sensor.fit_tomorrow","status":status,"reading":{"value":value,"unit":"kWh","observed_at":observed},"source_timestamps":{"last_updated":observed,"observed_at":observed},"source_attributes":{"fixture_issue_time":"not a verified provider issue time"}}]})
    }
    #[test]
    fn schema_migration_is_atomic_idempotent_and_rejects_newer_versions() {
        let mut db = Connection::open_in_memory().unwrap();
        migrate(&mut db).unwrap();
        migrate(&mut db).unwrap();
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        for table in ["samples", "snapshots", "prices"] {
            db.prepare(&format!("SELECT * FROM {table}")).unwrap();
        }
        db.execute_batch("PRAGMA user_version=2").unwrap();
        assert!(migrate(&mut db).is_err());
        let mut broken = Connection::open_in_memory().unwrap();
        broken
            .execute_batch("CREATE TABLE snapshots(id INTEGER)")
            .unwrap();
        assert!(migrate(&mut broken).is_err());
        assert!(broken.prepare("SELECT * FROM samples").is_err()); // no half migration
        assert_eq!(
            broken
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn restart_and_duplicate_fetches_keep_first_seen_and_revisions() {
        let path = std::env::temp_dir().join(format!(
            "hems-history-{}-{}.sqlite3",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let cfg = config(path.to_string_lossy().into());
        let first = time("2026-10-24T22:30:00Z");
        let payload = forecast_payload("2026-10-24T22:00:00Z", "valid", json!(3.1));
        {
            let h = History::open(cfg.clone()).unwrap();
            h.record("forecasts", payload.clone(), first).await.unwrap();
        }
        let h = History::open(cfg).unwrap();
        h.record(
            "forecasts",
            payload.clone(),
            first + chrono::Duration::hours(2),
        )
        .await
        .unwrap();
        let mut revision = payload.clone();
        revision["forecasts"][0]["reading"]["value"] = json!(3.5);
        h.record("forecasts", revision, first + chrono::Duration::hours(3))
            .await
            .unwrap();
        let mut stale = payload;
        stale["forecasts"][0]["status"] = json!("stale");
        stale["forecasts"][0]["reading"] = Value::Null;
        h.record("forecasts", stale, first + chrono::Duration::hours(5))
            .await
            .unwrap();
        h.run(move |db| {
            assert_eq!(db.query_row("SELECT COUNT(*) FROM snapshots",[],|r|r.get::<_,i64>(0))?,3);
            let (seen,day,issued,observed): (i64,String,Option<String>,String) = db.query_row("SELECT first_seen,target_day,issued_at,observed_at FROM snapshots ORDER BY id LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            assert_eq!(seen,first.timestamp()); assert_eq!(day,"2026-10-25"); assert!(issued.is_none()); assert_eq!(observed,"2026-10-24T22:00:00Z");
            assert_eq!(db.query_row("SELECT COUNT(*) FROM snapshots WHERE status='stale' AND observed_at IS NOT NULL",[],|r|r.get::<_,i64>(0))?,1);
            Ok(())
        }).await.unwrap();
        drop(h);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn london_target_day_uses_source_update_even_when_fetched_after_midnight() {
        let fetched = time("2026-07-02T00:10:00Z");
        let payload = forecast_payload("2026-07-01T22:30:00Z", "stale", Value::Null);
        assert_eq!(target_day(&payload["forecasts"][0], fetched), "2026-07-02");
        let payload = forecast_payload("2026-07-01T23:30:00Z", "valid", json!(4));
        assert_eq!(target_day(&payload["forecasts"][0], fetched), "2026-07-03"); // BST date already July 2
    }
    #[tokio::test]
    async fn sample_identity_and_provenance_keep_assumptions_and_stale_gaps() {
        let h = History::open(config(":memory:".into())).unwrap();
        let at = time("2026-10-28T10:00:00Z");
        let payload = json!({"fit_pv":{"status":"assumed_zero","source_status":"stale","reading":{"value":0,"unit":"W"},"source_timestamps":{"observed_at":"2026-10-28T00:00:00Z"}},"grid_net":{"status":"stale","reading":{"value":999,"unit":"W"}}});
        h.record("energy", payload.clone(), at).await.unwrap();
        h.record("energy", payload, at + chrono::Duration::seconds(10))
            .await
            .unwrap();
        h.record("battery",json!({"status":"stale","freshness":{"sources":{"soc_percent":{"status":"stale","observed_at":"2026-10-27T23:00:00Z"}}}}),at).await.unwrap();
        h.run(|db| {
            assert_eq!(db.query_row("SELECT COUNT(*) FROM samples",[],|r|r.get::<_,i64>(0))?,5);
            let (value,status,provenance): (f64,String,String) = db.query_row("SELECT value,status,provenance FROM samples WHERE metric='fit_pv'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            assert_eq!(value,0.0); assert_eq!(status,"assumed_zero"); assert_eq!(serde_json::from_str::<Value>(&provenance).unwrap()["source_status"],"stale");
            assert_eq!(db.query_row("SELECT COUNT(*) FROM samples WHERE value IS NULL",[],|r|r.get::<_,i64>(0))?,4);
            assert_eq!(db.query_row("SELECT observed_at FROM samples WHERE metric='soc_percent'",[],|r|r.get::<_,String>(0))?,"2026-10-27T23:00:00Z");
            let plan: String = db.query_row("EXPLAIN QUERY PLAN SELECT * FROM samples WHERE metric='fit_pv' AND sampled_at>=0 AND sampled_at<100 ORDER BY sampled_at",[],|r|r.get(3))?;
            assert!(plan.contains("INDEX")); Ok(())
        }).await.unwrap();
    }
    #[tokio::test]
    async fn price_intervals_deduplicate_and_retain_corrections() {
        let h = History::open(config(":memory:".into())).unwrap();
        let at = Utc::now();
        let mut payload = json!({"status":"schedule_available","entity_id":"event.agile","last_updated":at,"data":{"tariff_code":"E-1R-AGILE-TEST-A","intervals":[{"start":"2026-10-28T10:00:00Z","end":"2026-10-28T10:30:00Z","value_inc_vat":-0.1}]}});
        h.record("prices", payload.clone(), at).await.unwrap();
        h.record("prices", payload.clone(), at + chrono::Duration::minutes(5))
            .await
            .unwrap();
        payload["data"]["intervals"][0]["value_inc_vat"] = json!(0.25);
        h.record("prices", payload, at + chrono::Duration::minutes(10))
            .await
            .unwrap();
        h.run(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM prices", [], |r| r.get::<_, i64>(0))?,
                2
            );
            Ok(())
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn retention_removes_expired_rows_without_removing_recent_data() {
        let h = History::open(config(":memory:".into())).unwrap();
        for at in [Utc::now() - chrono::Duration::days(31), Utc::now()] {
            h.record(
                "energy",
                json!({"grid_net":{"status":"valid","reading":{"value":50}}}),
                at,
            )
            .await
            .unwrap();
            h.record(
                "forecasts",
                forecast_payload("2026-10-28T10:00:00Z", "valid", json!(at.timestamp())),
                at,
            )
            .await
            .unwrap();
        }
        h.prune().await.unwrap();
        h.run(|db| {
            for table in ["samples", "snapshots"] {
                assert_eq!(
                    db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))?,
                    1
                );
            }
            Ok(())
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn endpoints_bound_results_keep_provenance_and_filter_as_of() {
        let h = History::open(config(":memory:".into())).unwrap();
        let at = Utc::now() - chrono::Duration::minutes(10);
        for i in 0..3 {
            let time = at + chrono::Duration::minutes(i);
            h.record("energy",json!({"fit_pv":{"status":"assumed_zero","source_status":"stale","reading":{"value":0}}}),time).await.unwrap();
            h.record(
                "forecasts",
                forecast_payload("2026-10-28T10:00:00Z", "valid", json!(i)),
                time,
            )
            .await
            .unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router(h)).await.unwrap();
        });
        let client = reqwest::Client::new();
        let get = |path: String| client.get(format!("{base}{path}")).send();
        for path in [
            "/samples",
            "/samples?metric=x&limit=2001",
            "/samples?metric=x&from=0&to=9999999999",
            "/samples?metric=x&from=-1",
            "/snapshots?kind=arbitrary",
            "/samples?metric=x&unknown=true",
        ] {
            assert_eq!(
                get(path.into()).await.unwrap().status(),
                StatusCode::BAD_REQUEST
            );
        }
        let result: Value = get("/samples?metric=fit_pv&limit=2".into())
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["truncated"], true);
        assert_eq!(result["rows"].as_array().unwrap().len(), 2);
        assert_eq!(result["rows"][0]["provenance"]["source_status"], "stale");
        let result: Value = get(format!("/snapshots?as_of={}", at.timestamp()))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["rows"].as_array().unwrap().len(), 1);
        assert_eq!(result["rows"][0]["payload"]["reading"]["value"], 0);
        assert_eq!(
            get("/prices".into()).await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(
            get("/status".into()).await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(
            client
                .post(format!("{base}/samples"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        task.abort();
    }
    #[tokio::test]
    async fn collector_can_stop_during_an_upstream_outage() {
        let h = History::open(config(":memory:".into())).unwrap();
        let state = crate::api::AppState {
            ha: crate::home_assistant::HomeAssistantClient::new(
                "http://127.0.0.1:1".into(),
                "fixture".into(),
            )
            .unwrap(),
            collection: crate::api::CollectionConfig::from_env().unwrap(),
        };
        let (tx, rx) = tokio::sync::watch::channel(false);
        let worker = start(h, state, rx);
        tokio::task::yield_now().await;
        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
    }
}

#[cfg(test)]
mod revision_tests {
    use super::*;
    #[tokio::test]
    async fn unchanged_reports_deduplicate_but_reversions_and_quality_recovery_are_events() {
        let h = History::open(HistoryConfig {
            path: ":memory:".into(),
            interval: 60,
            days: 30,
        })
        .unwrap();
        let at = Utc::now();
        let mut f = json!({"forecasts":[{"array":"fit","horizon":"today","entity_id":"sensor.test","status":"valid","reading":{"value":3,"unit":"kWh"},"source_timestamps":{"last_updated":at,"last_reported":at,"observed_at":at}}]});
        h.record("forecasts", f.clone(), at).await.unwrap();
        f["forecasts"][0]["fetched_at"] = json!(at + chrono::Duration::minutes(1));
        f["forecasts"][0]["source_timestamps"]["last_reported"] =
            json!(at + chrono::Duration::minutes(1));
        f["forecasts"][0]["source_timestamps"]["observed_at"] =
            json!(at + chrono::Duration::minutes(1));
        h.record("forecasts", f.clone(), at + chrono::Duration::minutes(1))
            .await
            .unwrap();
        h.run(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM snapshots", [], |r| r.get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
        f["forecasts"][0]["status"] = json!("stale");
        h.record("forecasts", f.clone(), at + chrono::Duration::minutes(2))
            .await
            .unwrap();
        f["forecasts"][0]["status"] = json!("valid");
        h.record("forecasts", f, at + chrono::Duration::minutes(3))
            .await
            .unwrap();
        for (i, value) in [-0.1, 0.2, -0.1, -0.1].into_iter().enumerate() {
            h.record("prices",json!({"entity_id":"event.agile","status":"current_available","data":{"tariff_code":"E-1R-AGILE-TEST-A","intervals":[{"start":"2026-10-28T00:00:00Z","end":"2026-10-28T00:30:00Z","value_inc_vat":value}]}}),at+chrono::Duration::minutes(i as i64)).await.unwrap();
        }
        h.run(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM snapshots WHERE kind='forecasts'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                3
            );
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM prices", [], |r| r.get::<_, i64>(0))?,
                3
            );
            assert_eq!(
                db.query_row(
                    "SELECT value FROM prices ORDER BY id DESC LIMIT 1",
                    [],
                    |r| r.get::<_, f64>(0)
                )?,
                -0.1
            );
            Ok(())
        })
        .await
        .unwrap();
    }
    #[test]
    fn extreme_query_values_do_not_overflow() {
        assert!(
            HistoryQuery {
                to: Some(i64::MIN),
                ..Default::default()
            }
            .bounds()
            .is_err()
        );
        assert!(
            HistoryQuery {
                from: Some(0),
                to: Some(i64::MAX),
                ..Default::default()
            }
            .bounds()
            .is_err()
        );
    }
}
