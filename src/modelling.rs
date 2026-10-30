//! Read-only statistical foundation; no HA calls or model writes.
use crate::history::History;
use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use chrono::{DateTime, Datelike, Timelike, Utc};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
type Error = Box<dyn std::error::Error + Send + Sync>;
#[derive(Clone)]
pub struct Settings {
    nominal: f64,
    usable: f64,
    reserve: f64,
    base: f64,
    wfh: BTreeSet<u32>,
    dates: BTreeMap<String, String>,
}
impl Settings {
    pub fn from_env() -> Result<Self, Error> {
        fn n(k: &str, d: f64, min: f64, max: f64) -> Result<f64, Error> {
            let v = std::env::var(k).map_or(Ok(d), |s| s.parse::<f64>())?;
            if !v.is_finite() || v < min || v > max {
                return Err(format!("Invalid {k}").into());
            }
            Ok(v)
        }
        let nominal = n("HEMS_MODEL_NOMINAL_KWH", 10.24, 0.1, 1000.)?;
        let wfh = std::env::var("HEMS_MODEL_WFH_DAYS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().parse::<u32>())
            .collect::<Result<BTreeSet<_>, _>>()?;
        if wfh.iter().any(|d| !(1..=5).contains(d)) {
            return Err("WFH days must be ISO weekdays 1..5".into());
        }
        let dates: BTreeMap<String, String> = serde_json::from_str(
            &std::env::var("HEMS_MODEL_OCCUPANCY_DATES").unwrap_or_else(|_| "{}".into()),
        )?;
        for (day, label) in &dates {
            if day.parse::<chrono::NaiveDate>().is_err()
                || !matches!(label.as_str(), "home" | "away" | "wfh")
            {
                return Err(
                    "Occupancy dates require YYYY-MM-DD keys and home/away/wfh labels".into(),
                );
            }
        }
        Ok(Self {
            dates,
            nominal,
            usable: n("HEMS_MODEL_USABLE_KWH", 9.728, 0.1, nominal)?,
            reserve: n("HEMS_MODEL_RESERVE_PERCENT", 10., 0., 100.)?,
            base: n("HEMS_MODEL_ESSENTIAL_BASELOAD_W", 0., 0., 10000.)?,
            wfh,
        })
    }
    fn occupancy(&self, t: DateTime<Utc>) -> &str {
        let day = t
            .with_timezone(&chrono_tz::Europe::London)
            .date_naive()
            .to_string();
        if let Some(label) = self.dates.get(&day) {
            return label;
        }
        let d = t
            .with_timezone(&chrono_tz::Europe::London)
            .weekday()
            .number_from_monday();
        if d >= 6 {
            "weekend"
        } else if self.wfh.contains(&d) {
            "wfh_schedule"
        } else {
            "weekday_unknown"
        }
    }
}
#[derive(Clone)]
struct Sample {
    at: i64,
    metric: String,
    value: f64,
}
fn round(v: f64) -> f64 {
    (v * 10.).round() / 10.
}
fn percentile(v: &mut [f64], q: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[((v.len() - 1) as f64 * q).round() as usize])
}
fn load(db: &Connection, now: i64) -> Result<Vec<Sample>, Error> {
    let mut all = Vec::new();
    for metric in [
        "household",
        "soc_percent",
        "charge_power_w",
        "discharge_power_w",
        "fit_pv",
        "hybrid_pv_1",
        "hybrid_pv_2",
    ] {
        let mut stmt=db.prepare("SELECT sampled_at,value,observed_at,unit,status FROM samples WHERE metric=?1 AND sampled_at>=?2 AND sampled_at<=?3 ORDER BY sampled_at LIMIT 40321")?;
        let rows = stmt.query_map(params![metric, now - 28 * 86400, now], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        for r in rows {
            let (at, v, obs, unit, status) = r?;
            let Some(v) = v.filter(|v| v.is_finite() && *v >= 0.) else {
                continue;
            };
            let age = obs
                .and_then(|s| s.parse::<DateTime<Utc>>().ok())
                .map(|t| at - t.timestamp());
            if !matches!(
                status.as_str(),
                "valid" | "fresh" | "derived" | "future_within_tolerance"
            ) || !age.is_some_and(|a| (-60..=300).contains(&a))
                || unit.as_deref() != Some(if metric == "soc_percent" { "%" } else { "W" })
                || (metric == "soc_percent" && v > 100.)
            {
                continue;
            }
            all.push(Sample {
                at,
                metric: metric.into(),
                value: v,
            });
        }
    }
    Ok(all)
}
fn profiles(samples: &[Sample], cfg: &Settings) -> Value {
    // UTC hours remain separate through DST folds; group means give days equal weight.
    let mut hours: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for s in samples.iter().filter(|s| s.metric == "household") {
        hours.entry(s.at / 3600 * 3600).or_default().push(s.value);
    }
    let mut groups: BTreeMap<(u32, String), BTreeMap<String, Vec<f64>>> = BTreeMap::new();
    for (at, v) in hours {
        if v.len() < 30 {
            continue;
        }
        let t = DateTime::from_timestamp(at, 0).unwrap();
        let l = t.with_timezone(&chrono_tz::Europe::London);
        groups
            .entry((l.hour(), cfg.occupancy(t).into()))
            .or_default()
            .entry(l.date_naive().to_string())
            .or_default()
            .push(v.iter().sum::<f64>() / v.len() as f64);
    }
    let mut out = Vec::new();
    for ((hour, o), days) in groups {
        let mut v: Vec<f64> = days
            .values()
            .map(|v| v.iter().sum::<f64>() / v.len() as f64)
            .collect();
        out.push(json!({"hour":hour,"period":match hour{0..=6=>"overnight",7..=16=>"daytime",_=>"evening"},"occupancy":o,"days":v.len(),"ready":v.len()>=3,"low_w":round(percentile(&mut v,0.1).unwrap().max(cfg.base)),"median_w":round(percentile(&mut v,0.5).unwrap().max(cfg.base)),"high_w":round(percentile(&mut v,0.9).unwrap().max(cfg.base))}));
    }
    json!(out)
}
fn model(samples: &[Sample], cfg: &Settings, now: i64) -> Value {
    let profiles = profiles(samples, cfg);
    let latest = |metric: &str| {
        samples
            .iter()
            .filter(|s| s.metric == metric && now - s.at <= 300)
            .max_by_key(|s| s.at)
            .map(|s| s.value)
    };
    let soc = latest("soc_percent");
    let floor = soc.map(|s| s.min(cfg.reserve)).unwrap_or(cfg.reserve);
    let energy = soc.map(|s| cfg.usable * (s - cfg.reserve).max(0.) / 100.);
    let mut e = energy;
    let mut low = energy;
    let mut high = energy;
    let mut predictions = Vec::new();
    // UTC half-hour boundaries preserve elapsed duration through DST transitions.
    for step in 0..6 {
        let at = if step == 0 {
            now
        } else {
            now / 1800 * 1800 + step * 1800
        };
        let end = now / 1800 * 1800 + (step + 1) * 1800;
        let hours = (end - at) as f64 / 3600.;
        let t = DateTime::from_timestamp(at, 0).unwrap();
        let l = t.with_timezone(&chrono_tz::Europe::London);
        let o = cfg.occupancy(t);
        let row = profiles
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["hour"] == l.hour() && r["occupancy"] == o && r["ready"] == true);
        let demand = row.and_then(|r| r["median_w"].as_f64());
        // Integrate the first partial half hour using its local-hour profile.

        e = e.zip(demand).map(|(e, w)| (e - w * hours / 1000.).max(0.));
        low = low
            .zip(row.and_then(|r| r["high_w"].as_f64()))
            .map(|(e, w)| (e - w * hours / 1000.).max(0.));
        high = high
            .zip(row.and_then(|r| r["low_w"].as_f64()))
            .map(|(e, w)| (e - w * hours / 1000.).max(0.));
        predictions.push(json!({"start":t,"end":DateTime::from_timestamp(end,0),"local_start":l.to_rfc3339(),"occupancy":o,"demand_w":demand,"low_w":row.map(|r|r["low_w"].clone()),"high_w":row.map(|r|r["high_w"].clone()),"soc_scenario_end_percent":e.map(|e|round(floor+e/cfg.usable*100.)),"soc_scenario_low_percent":low.map(|e|round(floor+e/cfg.usable*100.)),"soc_scenario_high_percent":high.map(|e|round(floor+e/cfg.usable*100.))}));
    }
    let rates = |metric: &str| {
        let mut v: Vec<_> = samples
            .iter()
            .filter(|s| s.metric == metric && s.value > 0.)
            .map(|s| s.value)
            .collect();
        let enough = v.len() >= 30;
        json!({"samples":v.len(),"minimum_samples":30,"median_active_w":if enough{percentile(&mut v,0.5).map(round)}else{None},"p90_active_w":if enough{percentile(&mut v,0.9).map(round)}else{None},"basis":"reported active power, not an inverter limit"})
    };
    let mut counts = BTreeMap::new();
    for sample in samples {
        *counts.entry(sample.metric.clone()).or_insert(0usize) += 1;
    }
    json!({"as_of":DateTime::from_timestamp(now,0),"timezone":"Europe/London","advisory_only":true,"battery":{"nominal_kwh":cfg.nominal,"assumed_usable_kwh":cfg.usable,"reserve_percent":cfg.reserve,"soc_percent":soc,"stored_kwh":soc.map(|s|round(cfg.usable*s/100.)),"above_reserve_kwh":energy.map(round),"charge":rates("charge_power_w"),"discharge":rates("discharge_power_w"),"measured_efficiency":null,"efficiency_reason":"No independently verified same-boundary energy channels. SOC quantisation and AC/DC mixing prevent measured efficiency claims."},"profiles":profiles,"predictions":predictions,"diagnostics":{"accepted_samples":samples.len(),"accepted_by_metric":counts,"training_days":28,"minimum_days_per_hour":3,"minimum_samples_per_hour":30,"essential_baseload_w":cfg.base,"occupancy_basis":"Explicit date labels override configured WFH weekday schedule; other weekdays unknown. Schedule assumptions also apply to historical dates. No inferred presence or away savings.","uncertainty":"Daily hourly means, descriptive 10th–90th percentiles, not calibrated intervals. Sparse groups withheld.","soc_scenario":"No solar, no charging, lossless unlimited discharge; optimistic demand-only scenario. Reserve is a floor, grid supplies unmet load. Initial partial interval integrated proportionally.","missing_data":"Reject assumed zero, stale, missing units/timestamps, source reports older than 300s. Latest SOC expires after 300s.","assumptions":"Published usable capacity is configurable, not measured. SOC maps linearly to usable capacity; confirm SOC scale and reserve. CCTV/network remain in whole-home demand plus configured essential floor."}})
}
fn solar(db: &Connection, samples: &[Sample], now: i64) -> Result<Value, Error> {
    let today = DateTime::from_timestamp(now, 0)
        .unwrap()
        .with_timezone(&chrono_tz::Europe::London)
        .date_naive();
    let mut out = Vec::new();
    for days in 1..=7 {
        let day = today - chrono::Duration::days(days);
        let start = day
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(chrono_tz::Europe::London)
            .single()
            .unwrap()
            .timestamp();
        let end = (day + chrono::Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(chrono_tz::Europe::London)
            .single()
            .unwrap()
            .timestamp();
        for (array, metric, basis) in [
            ("fit", "fit_pv", "AC output"),
            ("hybrid_1", "hybrid_pv_1", "DC input NW"),
            ("hybrid_2", "hybrid_pv_2", "DC input W"),
        ] {
            let mut stmt=db.prepare("SELECT first_seen,issued_at,payload FROM snapshots WHERE kind='forecasts' AND target_day=?1 AND first_seen<?2 AND substr(source,1,instr(source,':')-1)=?3 ORDER BY first_seen DESC,id DESC LIMIT 1")?;
            let mut rows = stmt.query(params![day.to_string(), start, array])?;
            let forecast = if let Some(r) = rows.next()? {
                let seen: i64 = r.get(0)?;
                let issued: Option<String> = r.get(1)?;
                let p: Value = serde_json::from_str(&r.get::<_, String>(2)?)?;
                json!({"first_seen":DateTime::from_timestamp(seen,0),"provider_issued_at":issued,"kwh":if p["status"]=="valid"&&p["reading"]["unit"]=="kWh"{p["reading"]["value"].clone()}else{Value::Null}})
            } else {
                Value::Null
            };
            let s: Vec<_> = samples
                .iter()
                .filter(|s| s.metric == metric && s.at >= start && s.at < end)
                .collect();
            let (mut energy, mut covered) = (0., 0i64);
            for pair in s.windows(2) {
                let dt = pair[1].at - pair[0].at;
                if dt > 0 && dt <= 120 {
                    energy += (pair[0].value + pair[1].value) / 2. * dt as f64 / 3_600_000.;
                    covered += dt;
                }
            }
            let coverage = covered as f64 / (end - start) as f64;
            let actual = if coverage >= 0.95 {
                Some(round(energy))
            } else {
                None
            };
            let predicted = forecast["kwh"]
                .as_f64()
                .filter(|v| v.is_finite() && *v >= 0.);
            out.push(json!({"day":day.to_string(),"array":array,"basis":basis,"forecast":forecast,"coverage_percent":round(coverage*100.),"actual_estimate_kwh":actual,"error_actual_minus_forecast_kwh":actual.zip(predicted).map(|(a,f)|round(a-f)),"note":"Pre-local-midnight first-seen forecast only; provider issuance may be unknown. Power integration requires 95% full-day coverage with gaps <=120s. AC/DC basis differs; no combined battery charging inference."}));
        }
    }
    Ok(json!(out))
}
#[derive(Clone)]
struct ModelState {
    history: History,
    cfg: Settings,
    cache: Arc<tokio::sync::Mutex<Option<(i64, Value)>>>,
}
async fn summary(State(s): State<ModelState>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut cache = s.cache.try_lock().map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"Model busy"})),
        )
    })?;
    let now = Utc::now().timestamp();
    // Single-flight cache bounds SQLite work even under concurrent manual refreshes.
    if let Some((at, v)) = &*cache
        && now - *at < 60
    {
        return Ok(Json(v.clone()));
    }
    let cfg = s.cfg.clone();
    let value = s
        .history
        .run(move |db| {
            let samples = load(db, now)?;
            let mut v = model(&samples, &cfg, now);
            v["solar_comparisons"] = solar(db, &samples, now)?;
            Ok(v)
        })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"Modelling history unavailable"})),
            )
        })?;
    *cache = Some((now, value.clone()));
    Ok(Json(value))
}
pub fn router(history: History, cfg: Settings) -> Router {
    Router::new()
        .route("/summary", get(summary))
        .with_state(ModelState {
            history,
            cfg,
            cache: Arc::new(tokio::sync::Mutex::new(None)),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cfg() -> Settings {
        Settings {
            nominal: 10.24,
            usable: 9.728,
            reserve: 10.,
            base: 150.,
            wfh: BTreeSet::from([1]),
            dates: BTreeMap::new(),
        }
    }
    fn ts(s: &str) -> i64 {
        s.parse::<DateTime<Utc>>().unwrap().timestamp()
    }
    #[test]
    fn sparse_and_stale_soc_withhold_estimates() {
        let now = ts("2026-10-30T12:00:00Z");
        let v = model(&[], &cfg(), now);
        assert!(v["battery"]["soc_percent"].is_null());
        assert!(v["predictions"][0]["demand_w"].is_null());
        let s = Sample {
            at: now - 301,
            metric: "soc_percent".into(),
            value: 60.,
        };
        assert!(model(&[s], &cfg(), now)["battery"]["stored_kwh"].is_null());
    }
    #[test]
    fn occupancy_floor_and_daily_coverage() {
        let mut s = Vec::new();
        for (day, w) in [
            ("2026-10-05", 500.),
            ("2026-10-12", 600.),
            ("2026-10-19", 700.),
            ("2026-10-06", 50.),
            ("2026-10-13", 60.),
            ("2026-10-20", 70.),
        ] {
            let at = ts(&format!("{day}T11:00:00Z"));
            for m in 0..60 {
                s.push(Sample {
                    at: at + m * 60,
                    metric: "household".into(),
                    value: w,
                });
            }
        }
        let p = profiles(&s, &cfg());
        let rows = p.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let wfh = rows
            .iter()
            .find(|r| r["occupancy"] == "wfh_schedule")
            .unwrap();
        assert_eq!(wfh["median_w"], 600.);
        let unknown = rows
            .iter()
            .find(|r| r["occupancy"] == "weekday_unknown")
            .unwrap();
        assert_eq!(unknown["median_w"], 150.);
        assert_eq!(unknown["ready"], true);
    }
    #[test]
    fn dst_labels_skip_and_repeat_with_offsets() {
        for (now, expected) in [
            ("2026-03-29T00:30:00Z", "2026-03-29T02:00:00+01:00"),
            ("2026-10-25T00:30:00Z", "2026-10-25T01:00:00+00:00"),
        ] {
            let v = model(&[], &cfg(), ts(now));
            assert_eq!(v["predictions"][1]["local_start"], expected);
        }
    }
    #[tokio::test]
    async fn database_quality_solar_as_of_and_read_only_api() {
        let h = History::open(crate::history::HistoryConfig {
            path: ":memory:".into(),
            interval: 60,
            days: 30,
        })
        .unwrap();
        let now = ts("2026-10-30T12:00:00Z");
        h.run(move|db|{
 for(metric,status,obs,unit)in[("soc_percent","fresh",now,"%"),("household","valid",now-400,"W"),("fit_pv","assumed_zero",now,"W"),("hybrid_pv_1","valid",now,"kWh")]{db.execute("INSERT INTO samples VALUES (?1,?2,?3,?3,50,?4,?5,'{}')",params![now,metric,DateTime::from_timestamp(obs,0).unwrap().to_rfc3339(),unit,status])?;}
 let s=load(db,now)?;assert_eq!(s.len(),1);let v=model(&s,&cfg(),now);assert_eq!(v["battery"]["soc_percent"],50.);assert!(v["battery"]["measured_efficiency"].is_null());let comparisons=solar(db,&s,now)?;assert_eq!(comparisons.as_array().unwrap().len(),21);assert!(comparisons[0]["actual_estimate_kwh"].is_null());
 let before=ts("2026-10-28T20:00:00Z");for(seen,value)in[(before,3.),(now,9.)]{let p=json!({"status":"valid","reading":{"value":value,"unit":"kWh"}});db.execute("INSERT INTO snapshots(kind,source,target_day,first_seen,fetched_at,status,identity,payload) VALUES ('forecasts','fit:tomorrow:test','2026-10-29',?1,'fixture','valid',?2,?2)",params![seen,p.to_string()])?;}
 let v=solar(db,&s,now)?;assert_eq!(v[0]["forecast"]["kwh"],3.);assert!(v[0]["forecast"]["provider_issued_at"].is_null());Ok(())}).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/summary", listener.local_addr().unwrap());
        let task =
            tokio::spawn(async move { axum::serve(listener, router(h, cfg())).await.unwrap() });
        let client = reqwest::Client::new();
        let a: Value = client.get(&url).send().await.unwrap().json().await.unwrap();
        let b: Value = client.get(&url).send().await.unwrap().json().await.unwrap();
        assert_eq!(a, b);
        assert_eq!(
            client.post(&url).send().await.unwrap().status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        task.abort();
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    #[test]
    fn full_solar_days_use_23_and_25_elapsed_hours_and_gaps_are_withheld() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE snapshots(first_seen INTEGER,issued_at TEXT,payload TEXT,kind TEXT,target_day TEXT,source TEXT,id INTEGER)").unwrap();
        for (date, hours) in [("2026-03-29", 23), ("2026-10-25", 25)] {
            let day = date.parse::<chrono::NaiveDate>().unwrap();
            let start = day
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_local_timezone(chrono_tz::Europe::London)
                .unwrap()
                .timestamp();
            let end = start + hours * 3600;
            let s: Vec<_> = (start..end)
                .step_by(60)
                .map(|at| Sample {
                    at,
                    metric: "fit_pv".into(),
                    value: 1000.,
                })
                .collect();
            let now = end + 12 * 3600;
            let rows = solar(&db, &s, now).unwrap();
            assert_eq!(rows[0]["day"], date);
            assert_eq!(rows[0]["actual_estimate_kwh"], hours as f64);
            assert!(rows[1]["actual_estimate_kwh"].is_null());
            let sparse: Vec<_> = s
                .into_iter()
                .filter(|s| s.at < start + 3600 || s.at > start + 10800)
                .collect();
            assert!(solar(&db, &sparse, now).unwrap()[0]["actual_estimate_kwh"].is_null());
        }
    }
    #[test]
    fn date_labels_override_schedule_and_below_reserve_never_increases_soc() {
        let mut cfg = Settings {
            nominal: 10.24,
            usable: 9.728,
            reserve: 10.,
            base: 100.,
            wfh: BTreeSet::from([1]),
            dates: BTreeMap::from([("2026-10-26".into(), "away".into())]),
        };
        let now = "2026-10-26T12:00:00Z"
            .parse::<DateTime<Utc>>()
            .unwrap()
            .timestamp();
        assert_eq!(
            cfg.occupancy(DateTime::from_timestamp(now, 0).unwrap()),
            "away"
        );
        cfg.dates.clear();
        let mut samples = vec![Sample {
            at: now,
            metric: "soc_percent".into(),
            value: 5.,
        }];
        for day in ["2026-10-05", "2026-10-12", "2026-10-19"] {
            let at = format!("{day}T11:00:00Z")
                .parse::<DateTime<Utc>>()
                .unwrap()
                .timestamp();
            for m in 0..60 {
                samples.push(Sample {
                    at: at + m * 60,
                    metric: "household".into(),
                    value: 500.,
                });
            }
        }
        let v = model(&samples, &cfg, now);
        assert_eq!(v["predictions"][0]["demand_w"], 500.);
        assert_eq!(v["predictions"][0]["soc_scenario_end_percent"], 5.);
        assert_eq!(v["battery"]["above_reserve_kwh"], 0.);
    }
}
