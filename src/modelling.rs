//! Read-only statistical foundation; no HA calls or model writes.
mod daylight;
mod projection;

use crate::history::{History, MODELLING_SAMPLE_FILTER};
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
    projection: projection::Settings,
}
fn n(k: &str, d: f64, min: f64, max: f64) -> Result<f64, Error> {
    let v = std::env::var(k).map_or(Ok(d), |s| s.parse::<f64>())?;
    if !v.is_finite() || v < min || v > max {
        return Err(format!("Invalid {k}").into());
    }
    Ok(v)
}
impl Settings {
    pub fn from_env() -> Result<Self, Error> {
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
            projection: projection::Settings::from_env()?,
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
    observed: i64,
    fetched: i64,
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
    let mut stmt = db.prepare(&format!(
        "SELECT sampled_at,metric,value,observed_at,unit,fetched_at
         FROM samples INDEXED BY samples_modelling
         WHERE sampled_at BETWEEN ?1 AND ?2 AND {MODELLING_SAMPLE_FILTER}
         ORDER BY sampled_at LIMIT 400000",
    ))?;
    let mut rows = stmt.query(params![now - 28 * 86400, now])?;
    let mut all = Vec::new();
    while let Some(row) = rows.next()? {
        let at: i64 = row.get(0)?;
        let metric: String = row.get(1)?;
        let value: f64 = row.get(2)?;
        let soc = metric == "soc_percent";
        if !value.is_finite() || (value < 0. && metric != "grid_net") || (soc && value > 100.) {
            continue;
        }
        let unit: Option<String> = row.get(4)?;
        if unit.as_deref() != Some(if soc { "%" } else { "W" }) {
            continue;
        }
        let observed = row
            .get::<_, Option<String>>(3)?
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
            .map(|t| t.timestamp());
        let Some(observed) = observed.filter(|t| (-60..=300).contains(&(at - t))) else {
            continue;
        };
        let fetched = row
            .get::<_, String>(5)?
            .parse::<DateTime<Utc>>()
            .map_or(i64::MAX, |t| t.timestamp());
        all.push(Sample {
            at,
            metric,
            observed,
            fetched,
            value,
        });
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
#[cfg(test)]
fn model(samples: &[Sample], cfg: &Settings, now: i64) -> Value {
    let soc = samples
        .iter()
        .rev()
        .find(|s| s.metric == "soc_percent" && (0..=300).contains(&(now - s.observed)))
        .map(|s| s.value);
    model_with_soc(samples, cfg, now, soc)
}

fn model_with_soc(samples: &[Sample], cfg: &Settings, now: i64, soc: Option<f64>) -> Value {
    let profiles = profiles(samples, cfg);
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
    json!({"as_of":DateTime::from_timestamp(now,0),"timezone":"Europe/London","advisory_only":true,"battery":{"nominal_kwh":cfg.nominal,"assumed_usable_kwh":cfg.usable,"reserve_percent":cfg.reserve,"soc_percent":soc,"stored_kwh":soc.map(|s|round(cfg.usable*s/100.)),"above_reserve_kwh":energy.map(round),"charge":rates("charge_power_w"),"discharge":rates("discharge_power_w"),"measured_efficiency":null,"efficiency_reason":"No independently verified same-boundary energy channels. SOC quantisation and AC/DC mixing prevent measured efficiency claims."},"profiles":profiles,"predictions":predictions,"diagnostics":{"accepted_samples":samples.len(),"accepted_by_metric":counts,"training_days":28,"minimum_days_per_hour":3,"minimum_samples_per_hour":30,"essential_baseload_w":cfg.base,"occupancy_basis":"Explicit date labels override configured WFH weekday schedule; other weekdays unknown. Schedule assumptions also apply to historical dates. No inferred presence or away savings.","uncertainty":"Daily hourly means, descriptive 10th–90th percentiles, not calibrated intervals. Sparse groups withheld.","soc_scenario":"Demand-only SOC uses an hourly demand profile split into half-hours, with no solar or charging, no conversion losses and no discharge power limit. Discharge stops at reserve. Grid supplies unmet load. A missing demand interval stops subsequent SOC estimates.","missing_data":"Reject assumed zero, stale, missing units/timestamps, source reports older than 300s. Solis SOC uses the latest collector result with its original Home Assistant timestamp.","assumptions":"Published usable capacity is configurable, not measured. SOC maps linearly to usable capacity; confirm SOC scale and reserve. CCTV/network remain in whole-home demand plus configured essential floor."}})
}
fn solar(
    db: &Connection,
    samples: &[Sample],
    now: i64,
    site: Option<(f64, f64)>,
) -> Result<Value, Error> {
    let today = DateTime::from_timestamp(now, 0)
        .unwrap()
        .with_timezone(&chrono_tz::Europe::London)
        .date_naive();
    let arrays = [
        ("fit", "fit_pv", "AC output"),
        ("hybrid_1", "hybrid_pv_1", "DC input NW"),
        ("hybrid_2", "hybrid_pv_2", "DC input W"),
    ];
    let mut series: [Vec<&Sample>; 3] = Default::default();
    for sample in samples.iter().rev().take_while(|s| s.at >= now - 9 * 86400) {
        if sample.fetched <= now
            && let Some(i) = arrays
                .iter()
                .position(|&(_, metric, _)| metric == sample.metric)
        {
            series[i].push(sample);
        }
    }
    for readings in &mut series {
        readings.reverse();
    }
    let mut stmt = db.prepare("SELECT first_seen,issued_at,payload FROM snapshots WHERE kind='forecasts' AND target_day=?1 AND first_seen<?2 AND substr(source,1,instr(source,':')-1)=?3 ORDER BY first_seen DESC,id DESC LIMIT 1")?;
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
        let windows = daylight::relevant_windows(start..end, site);
        let relevant: i64 = windows.iter().map(|w| w.end - w.start).sum();
        let coverage_basis = match (site, relevant) {
            (None, _) => "full_day",
            (_, 0) => "no_daylight",
            _ => "daylight_and_twilight",
        };
        for ((array, _, basis), readings) in arrays.into_iter().zip(&series) {
            let mut rows = stmt.query(params![day.to_string(), start, array])?;
            let forecast = if let Some(r) = rows.next()? {
                let seen: i64 = r.get(0)?;
                let issued: Option<String> = r.get(1)?;
                let p: Value = serde_json::from_str(&r.get::<_, String>(2)?)?;
                json!({"first_seen":DateTime::from_timestamp(seen,0),"provider_issued_at":issued,"kwh":if p["status"]=="valid"&&p["reading"]["unit"]=="kWh"{p["reading"]["value"].clone()}else{Value::Null}})
            } else {
                Value::Null
            };
            let (energy, covered) = solar_energy(readings, &windows);
            let coverage = (relevant > 0).then(|| covered as f64 / relevant as f64);
            let actual = coverage.filter(|&covered| covered >= 0.95).map(|_| energy);
            let predicted = forecast["kwh"]
                .as_f64()
                .filter(|v| v.is_finite() && *v >= 0.);
            out.push(json!({
                "day": day.to_string(), "array": array, "basis": basis, "forecast": forecast,
                "coverage_basis": coverage_basis, "coverage_percent": coverage.map(|c| (c * 1000.).floor() / 10.),
                "covered_seconds": covered, "relevant_seconds": relevant,
                "actual_estimate_kwh": actual.map(round),
                "error_actual_minus_forecast_kwh": actual.zip(predicted).map(|(a, f)| round(a - f)),
                "note": "Actuals integrate accepted power readings up to two minutes apart and require 95% coverage of daylight including civil twilight. Uncovered time is not extrapolated. With no site coordinates, coverage uses the full local day. With no daylight, measured actuals and forecast errors are withheld. Forecasts use snapshots seen before London midnight; provider issuance may be unknown. Compare each array on its own AC or DC basis.",
            }));
        }
    }
    Ok(json!(out))
}
fn solar_energy(readings: &[&Sample], windows: &[std::ops::Range<i64>]) -> (f64, i64) {
    let (mut energy, mut covered) = (0., 0);
    for window in windows {
        let first = readings
            .partition_point(|s| s.at < window.start)
            .saturating_sub(1);
        let last = (readings.partition_point(|s| s.at < window.end) + 1).min(readings.len());
        for pair in readings[first..last].windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let dt = b.at - a.at;
            let start = a.at.max(window.start);
            let end = b.at.min(window.end);
            if !(1..=120).contains(&dt) || end <= start {
                continue;
            }
            let watts_at = |at| a.value + (b.value - a.value) * (at - a.at) as f64 / dt as f64;
            energy += (watts_at(start) + watts_at(end)) / 2. * (end - start) as f64 / 3_600_000.;
            covered += end - start;
        }
    }
    (energy, covered)
}

// Read the newest collector result, including invalid rows, so an explicit
// unavailable state cannot be hidden by an earlier numeric report.
fn latest_inverter_soc(db: &Connection, now: i64, max_poll_age: i64) -> Result<Value, Error> {
    let mut stmt = db.prepare(
        "SELECT value, unit, status, fetched_at, observed_at, provenance FROM samples
         WHERE metric = 'soc_percent' AND sampled_at BETWEEN ?1 AND ?2
         ORDER BY sampled_at DESC LIMIT 1",
    )?;
    let mut rows = stmt.query(params![now - 2 * max_poll_age, now])?;
    let Some(row) = rows.next()? else {
        return Ok(json!({"status":"collector_stale"}));
    };
    let fetched: String = row.get(3)?;
    let observed: Option<String> = row.get(4)?;
    let timestamp = |s: &str| s.parse::<DateTime<Utc>>().ok().map(|t| t.timestamp());
    let Some(fetched_at) = timestamp(&fetched).filter(|t| (0..=max_poll_age).contains(&(now - t)))
    else {
        return Ok(json!({"status":"collector_stale"}));
    };
    let status: String = row.get(2)?;
    let value: Option<f64> = row.get(0)?;
    let unit: Option<String> = row.get(1)?;
    let Some(value) = value.filter(|v| {
        (0.0..=100.0).contains(v)
            && unit.as_deref() == Some("%")
            && matches!(
                status.as_str(),
                "fresh"
                    | "last_known"
                    | "older_report_accepted_via_group"
                    | "future_within_tolerance"
            )
    }) else {
        return Ok(json!({"status":status,"fetched_at":fetched}));
    };
    let Some(observed_at) = observed
        .as_deref()
        .and_then(timestamp)
        .filter(|t| *t <= fetched_at + 30)
    else {
        return Ok(json!({"status":"invalid_timestamp","fetched_at":fetched}));
    };
    let age = now - observed_at;
    let provenance =
        serde_json::from_str::<Value>(&row.get::<_, String>(5)?).unwrap_or(Value::Null);
    Ok(json!({
        "value":value,"entity_id":crate::home_assistant::BATTERY_IDS[0],
        "status":if age > 300 {"last_known"} else if age < 0 {"future_within_tolerance"} else {"recent_ha_report"},
        "observed_at":observed,"age_seconds":age,"fetched_at":fetched,
        "observed_at_source":provenance["reading"]["observed_at_source"],
        "last_changed":provenance["reading"]["last_changed"],
        "last_updated":provenance["reading"]["last_updated"],
        "last_reported":provenance["reading"]["last_reported"],
    }))
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
    let max_poll_age = (2 * s.history.config.interval as i64).max(300);
    let value = s
        .history
        .run(move |db| {
            let samples = load(db, now)?;
            let soc = latest_inverter_soc(db, now, max_poll_age)?;
            let mut v = model_with_soc(&samples, &cfg, now, soc["value"].as_f64());
            v["projection"] = projection::calculate(&samples, &cfg, now, &soc);
            v["solar_comparisons"] = solar(db, &samples, now, cfg.projection.site)?;
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
    pub(super) fn cfg() -> Settings {
        Settings {
            projection: projection::Settings::default(),
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
    #[tokio::test]
    async fn current_collector_result_keeps_old_soc_and_latest_invalid_state_wins() {
        let history = History::open(crate::history::HistoryConfig {
            path: ":memory:".into(),
            interval: 60,
            days: 30,
        })
        .unwrap();
        let now = Utc::now().timestamp().div_euclid(60) * 60;
        let fetched = DateTime::from_timestamp(now, 0).unwrap();
        let old = fetched - chrono::Duration::hours(12);
        history.record("battery", json!({
            "soc_percent":98.,
            "freshness":{"fetched_at":fetched,"sources":{"soc_percent":{
                "status":"last_known","observed_at":old,"last_updated":old,"observed_at_source":"last_updated"
            }}}
        }), fetched).await.unwrap();
        history
            .run(move |db| {
                let source = latest_inverter_soc(db, now, 300)?;
                assert_eq!(source["value"], 98.);
                assert_eq!(source["status"], "last_known");
                assert_eq!(source["age_seconds"], 43200);
                assert_eq!(source["observed_at_source"], "last_updated");
                assert_eq!(source["observed_at"], json!(old));
                let projection = projection::calculate(&[], &cfg(), now + 5, &source);
                assert_eq!(projection["initial_soc_percent"], 98.);
                assert_eq!(projection["available_intervals"], 49);
                assert_eq!(
                    latest_inverter_soc(db, now + 301, 300)?["status"],
                    "collector_stale"
                );
                Ok(())
            })
            .await
            .unwrap();
        let later = fetched + chrono::Duration::minutes(1);
        history
            .record(
                "battery",
                json!({"freshness":{"fetched_at":later,"sources":{
                    "soc_percent":{"status":"invalid_state"}
                }}}),
                later,
            )
            .await
            .unwrap();
        history
            .run(move |db| {
                let source = latest_inverter_soc(db, now + 60, 300)?;
                assert_eq!(source["status"], "invalid_state");
                assert!(source["value"].is_null());
                Ok(())
            })
            .await
            .unwrap();
    }

    #[test]
    fn sparse_and_stale_soc_withhold_estimates() {
        let now = ts("2026-10-30T12:00:00Z");
        let v = model(&[], &cfg(), now);
        assert!(v["battery"]["soc_percent"].is_null());
        assert!(v["predictions"][0]["demand_w"].is_null());
        let s = Sample {
            at: now - 301,
            observed: now - 301,
            fetched: now - 301,
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
                    observed: at + m * 60,
                    fetched: at + m * 60,
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
 let s=load(db,now)?;assert_eq!(s.len(),1);let v=model(&s,&cfg(),now);assert_eq!(v["battery"]["soc_percent"],50.);assert!(v["battery"]["measured_efficiency"].is_null());let comparisons=solar(db,&s,now,None)?;assert_eq!(comparisons.as_array().unwrap().len(),21);assert!(comparisons[0]["actual_estimate_kwh"].is_null());
 let before=ts("2026-10-28T20:00:00Z");for(seen,value)in[(before,3.),(now,9.)]{let p=json!({"status":"valid","reading":{"value":value,"unit":"kWh"}});db.execute("INSERT INTO snapshots(kind,source,target_day,first_seen,fetched_at,status,identity,payload) VALUES ('forecasts','fit:tomorrow:test','2026-10-29',?1,'fixture','valid',?2,?2)",params![seen,p.to_string()])?;}
 let v=solar(db,&s,now,None)?;assert_eq!(v[0]["forecast"]["kwh"],3.);assert!(v[0]["forecast"]["provider_issued_at"].is_null());Ok(())}).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/summary", listener.local_addr().unwrap());
        let task =
            tokio::spawn(async move { axum::serve(listener, router(h, cfg())).await.unwrap() });
        let client = reqwest::Client::new();
        let a: Value = client.get(&url).send().await.unwrap().json().await.unwrap();
        let b: Value = client.get(&url).send().await.unwrap().json().await.unwrap();
        assert_eq!(a, b);
        assert_eq!(a["projection"]["status"], "unavailable");
        assert_eq!(a["projection"]["backtest"].as_array().unwrap().len(), 3);
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
                    observed: at,
                    fetched: at,
                    metric: "fit_pv".into(),
                    value: 1000.,
                })
                .collect();
            let now = end + 12 * 3600;
            let rows = solar(&db, &s, now, None).unwrap();
            assert_eq!(rows[0]["day"], date);
            assert_eq!(rows[0]["actual_estimate_kwh"], hours as f64);
            assert!(rows[1]["actual_estimate_kwh"].is_null());
            let sparse: Vec<_> = s
                .into_iter()
                .filter(|s| s.at < start + 3600 || s.at > start + 10800)
                .collect();
            assert!(solar(&db, &sparse, now, None).unwrap()[0]["actual_estimate_kwh"].is_null());
        }
    }
    #[test]
    fn daylight_comparisons_require_measured_coverage_per_array() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE snapshots(first_seen INTEGER,issued_at TEXT,payload TEXT,kind TEXT,target_day TEXT,source TEXT,id INTEGER)").unwrap();
        let start = "2026-11-15T00:00:00Z"
            .parse::<DateTime<Utc>>()
            .unwrap()
            .timestamp();
        let end = start + 86400;
        let site = Some((51.5, -0.1));
        let windows = daylight::relevant_windows(start..end, site);
        let w = &windows[0];
        assert!(w.start > start && w.end < end);
        let mut samples = Vec::new();
        for at in (w.start..=w.end).step_by(60) {
            for (metric, value) in [("fit_pv", 1000.), ("hybrid_pv_1", 0.)] {
                samples.push(Sample {
                    at,
                    observed: at,
                    fetched: at,
                    value,
                    metric: metric.into(),
                });
            }
        }
        let now = end + 1;
        let rows = solar(&db, &samples, now, site).unwrap();
        assert_eq!(rows[0]["coverage_basis"], "daylight_and_twilight");
        assert_eq!(rows[0]["coverage_percent"], 100.);
        assert_eq!(
            rows[0]["actual_estimate_kwh"],
            round((w.end - w.start) as f64 / 3600.)
        );
        assert_eq!(rows[1]["actual_estimate_kwh"], 0.);
        assert_eq!(rows[1]["basis"], "DC input NW");
        assert_eq!(rows[2]["coverage_percent"], 0.);
        assert!(rows[2]["actual_estimate_kwh"].is_null());

        let fallback = solar(&db, &samples, now, None).unwrap();
        assert_eq!(fallback[0]["coverage_basis"], "full_day");
        assert_eq!(fallback[0]["relevant_seconds"], 86400);
        assert!(fallback[0]["actual_estimate_kwh"].is_null());

        samples.retain(|s| s.metric != "fit_pv" || s.at >= w.start + 3600);
        let partial = solar(&db, &samples, now, site).unwrap();
        assert!(partial[0]["coverage_percent"].as_f64().unwrap() < 95.);
        assert!(partial[0]["actual_estimate_kwh"].is_null());
        let polar = solar(&db, &[], now, Some((90., 0.))).unwrap();
        assert_eq!(polar[0]["coverage_basis"], "no_daylight");
        assert!(polar[0]["coverage_percent"].is_null());
        assert!(polar[0]["actual_estimate_kwh"].is_null());
    }

    #[test]
    fn solar_integration_clips_a_ramp_and_skips_long_gaps() {
        let readings: Vec<_> = [(0, 0.), (60, 1200.), (181, 1200.)]
            .into_iter()
            .map(|(at, value)| Sample {
                at,
                observed: at,
                fetched: at,
                value,
                metric: "fit_pv".into(),
            })
            .collect();
        let refs: Vec<_> = readings.iter().collect();
        let windows = std::iter::once(30..180).collect::<Vec<_>>();
        let (energy, covered) = solar_energy(&refs, &windows);
        assert_eq!(covered, 30);
        assert_eq!(energy, 900. * 30. / 3_600_000.);
    }

    #[test]
    fn date_labels_override_schedule_and_below_reserve_never_increases_soc() {
        let mut cfg = Settings {
            projection: projection::Settings::default(),
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
            observed: now,
            fetched: now,
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
                    observed: at + m * 60,
                    fetched: at + m * 60,
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
