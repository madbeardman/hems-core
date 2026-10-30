//! Octopus totals are authoritative amounts, not integrations of sampled power.
use crate::home_assistant::{EntityState, HomeAssistantClient};
use axum::{Json, Router, extract::State, routing::get};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone)]
pub struct CostsState {
    ha: HomeAssistantClient,
    sources: Vec<String>,
}
// Account/meter-specific IDs belong only in local configuration.
const INPUTS: &[(&str, &str, &str, &str)] = &[
    (
        "electricity_today_cost",
        "HEMS_ELECTRICITY_TODAY_COST_ENTITY",
        "GBP",
        "today",
    ),
    (
        "electricity_today_consumption",
        "HEMS_ELECTRICITY_TODAY_CONSUMPTION_ENTITY",
        "kWh",
        "today",
    ),
    (
        "electricity_standing_charge",
        "HEMS_ELECTRICITY_STANDING_CHARGE_ENTITY",
        "GBP",
        "tariff",
    ),
    (
        "electricity_previous_cost",
        "HEMS_ELECTRICITY_PREVIOUS_COST_ENTITY",
        "GBP",
        "previous",
    ),
    (
        "electricity_previous_consumption",
        "HEMS_ELECTRICITY_PREVIOUS_CONSUMPTION_ENTITY",
        "kWh",
        "previous",
    ),
    (
        "electricity_meter_demand",
        "HEMS_ELECTRICITY_METER_DEMAND_ENTITY",
        "W",
        "instant",
    ),
    (
        "gas_today_cost",
        "HEMS_GAS_TODAY_COST_ENTITY",
        "GBP",
        "today",
    ),
    (
        "gas_today_consumption",
        "HEMS_GAS_TODAY_CONSUMPTION_ENTITY",
        "kWh",
        "today",
    ),
    (
        "gas_standing_charge",
        "HEMS_GAS_STANDING_CHARGE_ENTITY",
        "GBP",
        "tariff",
    ),
    (
        "gas_previous_cost",
        "HEMS_GAS_PREVIOUS_COST_ENTITY",
        "GBP",
        "previous",
    ),
    (
        "gas_previous_consumption",
        "HEMS_GAS_PREVIOUS_CONSUMPTION_ENTITY",
        "kWh",
        "previous",
    ),
];
impl CostsState {
    pub fn from_env(ha: HomeAssistantClient) -> Result<Self, Box<dyn std::error::Error>> {
        let sources = INPUTS
            .iter()
            .map(|(_, variable, _, _)| {
                let id = std::env::var(variable).unwrap_or_default();
                if !id.is_empty()
                    && (!id.starts_with("sensor.")
                        || !id.bytes().all(|b| {
                            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.'
                        }))
                {
                    return Err(format!("Invalid entity ID in {variable}"));
                }
                Ok(id)
            })
            .collect::<Result<Vec<_>, String>>()?;
        let configured: Vec<_> = sources.iter().filter(|id| !id.is_empty()).collect();
        if configured
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != configured.len()
        {
            return Err("Cost mappings must use distinct sensors".into());
        }
        Ok(Self { ha, sources })
    }
}
fn stamp(value: &Value) -> Option<DateTime<Utc>> {
    value.as_str()?.parse().ok()
}
fn london_day(time: DateTime<Utc>) -> NaiveDate {
    time.with_timezone(&chrono_tz::Europe::London).date_naive()
}
fn adapt(entity: &EntityState, unit: &str, period: &str, now: DateTime<Utc>) -> Value {
    let attrs = &entity.attributes;
    let mut result = json!({"status":"invalid_state","reading":null,"fetched_at":now,
        "source_timestamps":{"last_updated":entity.last_updated,"last_changed":entity.last_changed,"last_reported":entity.last_reported,"observed_at":entity.last_reported.unwrap_or(entity.last_updated)},
        "period_kind":period,"source_metadata":{},"timestamp_basis":"Home Assistant reports; period dates come from Octopus attributes"});
    // Deliberately allowlist metadata: no account numbers or full charge arrays.
    for key in [
        "last_reset",
        "start",
        "end",
        "is_estimated",
        "latest_consumption_timestamp",
        "standing_charge",
        "total_without_standing_charge",
    ] {
        if let Some(value) = attrs.get(key) {
            result["source_metadata"][key] = value.clone();
        }
    }
    // Daily totals can update slowly; tariff values are validated by their
    // effective interval instead of telemetry age. Instant demand stays strict.
    let mut reading = match crate::home_assistant::validate_with_age(
        entity,
        now,
        if period == "instant" { Some(300) } else { None },
    ) {
        Ok(r) => r,
        Err(error) => {
            result["status"] = crate::home_assistant::reading_error_status(error.as_ref()).into();
            return result;
        }
    };
    let factor = match (reading.unit.as_deref(), unit) {
        (Some("GBP" | "£"), "GBP") | (Some("kWh"), "kWh") | (Some("W"), "W") => 1.0,
        (Some("p"), "GBP") => 0.01,
        (Some("Wh"), "kWh") => 0.001,
        (Some("kW"), "W") => 1000.0,
        _ => {
            result["status"] = "invalid_unit".into();
            return result;
        }
    };
    reading.value *= factor;
    reading.unit = Some(unit.into());
    if !reading.value.is_finite() || ((unit == "kWh" || period == "tariff") && reading.value < 0.0)
    {
        return result;
    }
    let status = if period == "instant" {
        "valid"
    } else if period == "tariff" {
        match (stamp(&attrs["start"]), stamp(&attrs["end"])) {
            (Some(start), end)
                if start <= now
                    && end.is_none_or(|end| now < end)
                    && (attrs["end"].is_null() || end.is_some()) =>
            {
                "valid"
            }
            _ => "tariff_period_unverified",
        }
    } else {
        let expected =
            london_day(now) - chrono::Duration::days(if period == "previous" { 1 } else { 0 });
        result["expected_day"] = json!(expected);
        let reset = stamp(&attrs["last_reset"]);
        let charges = attrs["charges"].as_array();
        let start = charges
            .and_then(|c| c.first())
            .and_then(|c| stamp(&c["start"]))
            .or(reset);
        let end = charges
            .and_then(|c| c.last())
            .and_then(|c| stamp(&c["end"]));
        result["period_start"] = json!(start);
        result["coverage_end"] = json!(end);
        result["coverage_note"] = json!(
            "Provider interval coverage; a partially populated interval is not proof of complete meter data"
        );
        if let Some(start) = start {
            let day = london_day(start);
            result["period_day"] = json!(day);
            let next_day = day.succ_opt().unwrap();
            let midnight = next_day
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_local_timezone(chrono_tz::Europe::London)
                .single()
                .unwrap()
                .with_timezone(&Utc);
            let consistent = reset.is_none_or(|r| london_day(r) == day)
                && end.is_none_or(|end| end > start && end <= midnight);
            if !consistent || day > expected {
                "period_mismatch"
            } else if day < expected {
                "delayed_period"
            } else {
                "valid"
            }
        } else {
            "period_unverified"
        }
    };
    result["status"] = json!(status);
    // Keep valid numeric delayed totals with their REAL date for diagnostics;
    // never let frontend combine them with today's totals.
    if matches!(status, "valid" | "delayed_period") {
        result["reading"] = serde_json::to_value(reading).unwrap();
    }
    result
}
pub async fn collect(state: CostsState) -> Value {
    // Fixed concurrency of three; four seconds per source bounds the endpoint
    // to sixteen seconds even when every configured sensor is unreachable.
    let mut entries = serde_json::Map::new();
    for offset in (0..INPUTS.len()).step_by(3) {
        let read = |index: usize| {
            let state = state.clone();
            async move {
                let Some(&(name, _, unit, period)) = INPUTS.get(index) else {
                    return None;
                };
                let id = state.sources.get(index).cloned().unwrap_or_default();
                let mut entry = if id.is_empty() {
                    json!({"status":"not_configured","reading":null})
                } else {
                    match tokio::time::timeout(Duration::from_secs(4), state.ha.entity(&id)).await {
                        Ok(Ok(entity)) => adapt(&entity, unit, period, Utc::now()),
                        Ok(Err(error)) => {
                            json!({"status":crate::home_assistant::diagnostic_status(error.as_ref()),"reading":null})
                        }
                        Err(_) => json!({"status":"ha_timeout","reading":null}),
                    }
                };
                entry["entity_id"] = json!(id);
                entry["fetched_at"] = json!(Utc::now());
                if name.ends_with("_cost") {
                    entry["standing_charge_included"] = json!(true);
                    entry["basis"] = json!(
                        "Octopus reported cumulative total including standing charge; do not add standing charge again"
                    );
                }
                if period == "instant" {
                    entry["basis"] = json!(
                        "Signed electricity meter demand, separate from inverter and household load; no integration into costs"
                    );
                }
                Some((name.to_owned(), entry))
            }
        };
        let (a, b, c) = tokio::join!(read(offset), read(offset + 1), read(offset + 2));
        for (name, entry) in [a, b, c].into_iter().flatten() {
            entries.insert(name, entry);
        }
    }
    json!({"fetched_at":Utc::now(),"timezone":"Europe/London","readings":entries,"basis":"Reported cumulative import costs including standing charges; no export credit or sampled-power cost estimate"})
}
async fn costs(State(state): State<CostsState>) -> Json<Value> {
    Json(collect(state).await)
}
pub fn router(state: CostsState) -> Router {
    Router::new().route("/costs", get(costs)).with_state(state)
}
pub fn start(
    state: CostsState,
    history: crate::history::History,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! { _ = stop.changed() => break, _ = tick.tick() => {} }
            tokio::select! {
                _ = stop.changed() => break,
                response = tokio::time::timeout(Duration::from_secs(50),collect(state.clone())) => match response {
                    Ok(payload) => if history.record("costs",payload["readings"].clone(),Utc::now()).await.is_err() { eprintln!("Cost history write failed"); },
                    Err(_) => eprintln!("Cost collection timed out"),
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn now() -> DateTime<Utc> {
        "2026-10-29T12:00:00Z".parse().unwrap()
    }
    fn entity(value: &str, unit: &str, attributes: Value) -> EntityState {
        let mut attrs = attributes;
        attrs["unit_of_measurement"] = json!(unit);
        serde_json::from_value(json!({"state":value,"attributes":attrs,"last_updated":now(),"last_changed":now(),"last_reported":now()})).unwrap()
    }
    #[test]
    fn normalizes_units_and_keeps_signed_costs_and_meter_demand() {
        let day = json!({"last_reset":"2026-10-29T00:00:00Z"});
        for (value, unit, target, period, attrs, expected) in [
            ("79", "p", "GBP", "today", day.clone(), 0.79),
            ("-0.2", "GBP", "GBP", "today", day.clone(), -0.2),
            ("363", "Wh", "kWh", "today", day.clone(), 0.363),
            ("-0.088", "kW", "W", "instant", json!({}), -88.0),
            ("0", "kWh", "kWh", "today", day, 0.0),
        ] {
            let result = adapt(&entity(value, unit, attrs), target, period, now());
            assert_eq!(result["status"], "valid");
            assert!((result["reading"]["value"].as_f64().unwrap() - expected).abs() < 1e-8);
        }
    }
    #[test]
    fn previous_means_provider_period_not_yesterdays_label() {
        let result = adapt(
            &entity(
                "1.7",
                "GBP",
                json!({"last_reset":"2026-10-27T00:00:00Z","charges":[{"start":"2026-10-27T00:00:00Z","end":"2026-10-28T00:00:00Z"}]}),
            ),
            "GBP",
            "previous",
            now(),
        );
        assert_eq!(result["status"], "delayed_period");
        assert_eq!(result["period_day"], "2026-10-27");
        assert_eq!(result["expected_day"], "2026-10-28");
        assert_eq!(result["reading"]["value"], 1.7);
    }
    #[test]
    fn rejects_unknown_future_or_conflicting_periods() {
        for attrs in [
            json!({}),
            json!({"last_reset":"2026-10-30T00:00:00Z"}),
            json!({"last_reset":"2026-10-28T00:00:00Z","charges":[{"start":"2026-10-29T00:00:00Z","end":"2026-10-29T00:30:00Z"}]}),
            json!({"last_reset":"2026-10-29T00:00:00Z","charges":[{"start":"2026-10-29T00:00:00Z","end":"2026-10-31T00:00:00Z"}]}),
        ] {
            let r = adapt(&entity("1", "GBP", attrs), "GBP", "today", now());
            assert!(r["reading"].is_null());
            assert_ne!(r["status"], "valid");
        }
    }
    #[test]
    fn daily_periods_respect_london_dst_and_keep_estimates_and_metadata_private() {
        let t = "2026-10-25T12:00:00Z".parse().unwrap();
        let mut e = entity(
            "8",
            "kWh",
            json!({"last_reset":"2026-10-25T00:00:00+01:00","charges":[{"start":"2026-10-25T00:00:00+01:00","end":"2026-10-26T00:00:00Z"}],"is_estimated":true,"mpan":"private-meter","serial_number":"private-serial"}),
        );
        e.last_updated = t;
        e.last_changed = t;
        e.last_reported = Some(t);
        let r = adapt(&e, "kWh", "today", t);
        assert_eq!(r["status"], "valid");
        assert_eq!(r["period_day"], "2026-10-25");
        assert_eq!(r["source_metadata"]["is_estimated"], true);
        assert!(!r.to_string().contains("private-"));
    }
    #[test]
    fn tariff_dates_allow_unchanged_standing_charges_but_not_expired_rates() {
        let mut e = entity(
            "0.631074",
            "GBP",
            json!({"start":"2024-10-01T00:00:00+01:00","end":null}),
        );
        e.last_updated = now() - chrono::Duration::days(400);
        e.last_changed = e.last_updated;
        e.last_reported = Some(e.last_updated);
        assert_eq!(adapt(&e, "GBP", "tariff", now())["status"], "valid");
        e.attributes["end"] = json!("2026-10-29T00:00:00Z");
        assert!(adapt(&e, "GBP", "tariff", now())["reading"].is_null());
    }
    #[test]
    fn demand_stales_out_and_invalid_readings_are_never_zeroed() {
        let mut e = entity("88", "W", json!({}));
        e.last_reported = Some(now() - chrono::Duration::seconds(301));
        e.last_updated = e.last_reported.unwrap();
        e.last_changed = e.last_updated;
        assert_eq!(adapt(&e, "W", "instant", now())["status"], "stale");
        for value in ["unavailable", "NaN", "inf", "-1"] {
            let result = adapt(
                &entity(value, "kWh", json!({"last_reset":"2026-10-29T00:00:00Z"})),
                "kWh",
                "today",
                now(),
            );
            assert!(result["reading"].is_null());
        }
        assert_eq!(
            adapt(&entity("1", "m³", json!({})), "kWh", "today", now())["status"],
            "invalid_unit"
        );
    }
    #[tokio::test]
    async fn endpoint_retains_total_without_adding_standing_charge_and_is_read_only() {
        let upstream = Router::new().route("/api/states/{id}",get(|| async { Json(json!({"state":"0.79","attributes":{"unit_of_measurement":"GBP","last_reset":Utc::now().with_timezone(&chrono_tz::Europe::London).date_naive().to_string()+"T00:00:00+00:00","standing_charge":0.63},"last_changed":Utc::now(),"last_updated":Utc::now(),"last_reported":Utc::now()})) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ha_url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let mut sources = vec![String::new(); INPUTS.len()];
        sources[0] = "sensor.cost_fixture".into();
        let state = CostsState {
            ha: HomeAssistantClient::new(ha_url, "fixture-token".into()).unwrap(),
            sources,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/costs", listener.local_addr().unwrap());
        let history = crate::history::History::open(crate::history::HistoryConfig {
            path: ":memory:".into(),
            interval: 60,
            days: 30,
        })
        .unwrap();
        let app = router(state).nest("/history", crate::history::router(history.clone()));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        let result: Value = client.get(&url).send().await.unwrap().json().await.unwrap();
        assert_eq!(
            result["readings"]["electricity_today_cost"]["reading"]["value"],
            0.79
        );
        assert_eq!(
            result["readings"]["electricity_today_cost"]["standing_charge_included"],
            true
        );
        assert_eq!(
            result["readings"]["gas_today_cost"]["status"],
            "not_configured"
        );
        assert!(!result.to_string().contains("fixture-token"));
        history
            .record("costs", result["readings"].clone(), Utc::now())
            .await
            .unwrap();
        let saved: Value = client
            .get(url.replace("/costs", "/history/samples?metric=electricity_today_cost"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(saved["rows"][0]["value"], 0.79);
        assert_eq!(
            saved["rows"][0]["provenance"]["standing_charge_included"],
            true
        );
        assert_eq!(
            saved["rows"][0]["provenance"]["period_day"],
            result["readings"]["electricity_today_cost"]["period_day"]
        );

        assert_eq!(
            client.post(url).send().await.unwrap().status(),
            axum::http::StatusCode::METHOD_NOT_ALLOWED
        );
        task.abort();
        server.abort();
    }
    #[tokio::test]
    async fn failures_are_generic_and_shutdown_is_bounded() {
        let state = CostsState {
            ha: HomeAssistantClient::new("http://127.0.0.1:1".into(), "private-token".into())
                .unwrap(),
            sources: vec!["sensor.fixture".into(); INPUTS.len()],
        };
        let result = collect(state.clone()).await;
        assert!(
            result["readings"]
                .as_object()
                .unwrap()
                .values()
                .all(|e| e["reading"].is_null())
        );
        assert!(!result.to_string().contains("private-token"));
        let h = crate::history::History::open(crate::history::HistoryConfig {
            path: ":memory:".into(),
            interval: 60,
            days: 30,
        })
        .unwrap();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let worker = start(state, h, rx);
        tokio::task::yield_now().await;
        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
    }
}
