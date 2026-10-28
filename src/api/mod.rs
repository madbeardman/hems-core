use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde::Serialize;

use crate::home_assistant::HomeAssistantClient;

#[derive(Clone)]
pub struct AppState {
    pub ha: HomeAssistantClient,
    pub collection: CollectionConfig,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
}

#[derive(Serialize)]
struct VersionResponse {
    service: &'static str,
    version: &'static str,
}

#[derive(Serialize)]
struct BatteryResponse {
    soc_percent: f64,
    charge_power_w: f64,
    discharge_power_w: f64,
}

#[derive(Serialize)]
struct ApiError {
    error: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy",
        service: "hems-core",
    })
}

async fn version() -> Json<VersionResponse> {
    Json(VersionResponse {
        service: "hems-core",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn battery(
    State(state): State<AppState>,
) -> Result<Json<BatteryResponse>, (StatusCode, Json<ApiError>)> {
    let [soc, charge, discharge] = state.ha.battery_snapshot().await.map_err(|_| {
        (
            StatusCode::BAD_GATEWAY,
            Json(ApiError {
                error: "Unable to retrieve battery data from Home Assistant",
            }),
        )
    })?;

    if !(0.0..=100.0).contains(&soc) || charge < 0.0 || discharge < 0.0 {
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(ApiError {
                error: "Home Assistant returned invalid battery data",
            }),
        ));
    }

    Ok(Json(BatteryResponse {
        soc_percent: soc,
        charge_power_w: charge,
        discharge_power_w: discharge,
    }))
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        .route("/battery", get(battery))
        .route("/energy", get(energy))
        .route("/prices", get(prices))
        .route("/forecasts", get(forecasts))
        .with_state(state)
}

#[derive(Clone)]
pub struct CollectionConfig {
    sources: std::collections::HashMap<&'static str, String>,
    rates_entity: String,
    pv_confirmed: bool,
    forecast_entities: Vec<String>,
    forecast_confirmed: bool,
}
impl CollectionConfig {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let defaults = [
            "sensor.solis_s6_gr1p_active_power",
            "sensor.solis_s6_eh1p_pv_power_1",
            "sensor.solis_s6_eh1p_pv_power_2",
        ];
        let mut sources = std::collections::HashMap::new();
        let mut custom_pv = false;
        for (index, &(name, variable, _)) in INPUTS.iter().enumerate() {
            let id = match std::env::var(variable) {
                Ok(id) => {
                    if index < 3 {
                        custom_pv = true;
                    }
                    id
                }
                Err(std::env::VarError::NotPresent) => {
                    defaults.get(index).unwrap_or(&"").to_string()
                }
                Err(_) => return Err("Collection entity ID must be Unicode".into()),
            };
            if !id.is_empty() && (!id.starts_with("sensor.") || !valid_id(&id)) {
                return Err(format!("Invalid entity ID in {variable}").into());
            }
            sources.insert(name, id);
        }
        let rates_entity = std::env::var("HEMS_AGILE_RATES_ENTITY").unwrap_or_default();
        if !rates_entity.is_empty()
            && (!rates_entity.starts_with("event.") || !valid_id(&rates_entity))
        {
            return Err("Invalid HEMS_AGILE_RATES_ENTITY".into());
        }
        let pv_confirmed = match std::env::var("HEMS_PV_MAPPINGS_CONFIRMED").as_deref() {
            Ok("true") => true,
            Ok("false") => false,
            Err(_) => !custom_pv,
            _ => return Err("HEMS_PV_MAPPINGS_CONFIRMED must be true or false".into()),
        };
        let forecast_entities = FORECAST_INPUTS
            .iter()
            .map(|(_, _, variable)| {
                let id = std::env::var(variable).unwrap_or_default();
                if !id.is_empty() && (!id.starts_with("sensor.") || !valid_id(&id)) {
                    return Err(format!("Invalid entity ID in {variable}"));
                }
                Ok(id)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let forecast_confirmed = match std::env::var("HEMS_FORECAST_MAPPINGS_CONFIRMED").as_deref()
        {
            Ok("true") => true,
            Ok("false") | Err(std::env::VarError::NotPresent) => false,
            _ => return Err("HEMS_FORECAST_MAPPINGS_CONFIRMED must be true or false".into()),
        };
        Ok(Self {
            forecast_entities,
            forecast_confirmed,
            sources,
            rates_entity,
            pv_confirmed,
        })
    }
}
fn valid_id(id: &str) -> bool {
    let Some((domain, object)) = id.split_once('.') else {
        return false;
    };
    !domain.is_empty()
        && !object.is_empty()
        && object
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

// Explicit, independent inputs only. Never add inverter AC output to PV inputs.
const INPUTS: &[(&str, &str, &str)] = &[
    ("fit_pv", "HEMS_FIT_PV_ENTITY", "W"),
    ("hybrid_pv_1", "HEMS_HYBRID_PV1_ENTITY", "W"),
    ("hybrid_pv_2", "HEMS_HYBRID_PV2_ENTITY", "W"),
    ("grid_import", "HEMS_GRID_IMPORT_ENTITY", "W"),
    ("grid_export", "HEMS_GRID_EXPORT_ENTITY", "W"),
    ("household", "HEMS_HOUSEHOLD_ENTITY", "W"),
];

fn normalize(
    reading: &mut crate::home_assistant::Reading,
    unit: &str,
    signed: bool,
) -> Result<(), &'static str> {
    let factor = match (reading.unit.as_deref(), unit) {
        (Some("kW"), "W") => 1000.0,
        (Some("p/kWh"), "GBP/kWh") => 0.01,
        (Some(actual), expected) if actual == expected => 1.0,
        _ => return Err("invalid_unit"),
    };
    reading.value *= factor;
    if !reading.value.is_finite() || (!signed && reading.value < 0.0) {
        return Err("invalid_value");
    }
    reading.unit = Some(unit.into());
    Ok(())
}

fn solar_total(pv: &[(f64, chrono::DateTime<chrono::Utc>)], confirmed: bool) -> Option<f64> {
    if !confirmed || pv.len() != 3 {
        return None;
    }
    let oldest = pv.iter().map(|(_, time)| time).min()?;
    let newest = pv.iter().map(|(_, time)| time).max()?;
    if *newest - *oldest > chrono::Duration::seconds(30) {
        return None;
    }
    let sum: f64 = pv.iter().map(|(value, _)| value).sum();
    if sum.is_finite() { Some(sum) } else { None }
}

async fn energy(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mut result = serde_json::Map::new();
    let mut seen = std::collections::HashSet::new();
    let mut pv = Vec::new();
    for &(name, _, unit) in INPUTS {
        let id = state
            .collection
            .sources
            .get(name)
            .cloned()
            .unwrap_or_default();
        let mut entry = serde_json::json!({"status": "not_configured", "value": null});
        if !id.is_empty() {
            if !seen.insert(id.clone()) {
                entry["status"] = "duplicate_entity".into();
            } else {
                match state.ha.reading(&id).await {
                    Ok(mut reading) => match normalize(&mut reading, unit, false) {
                        Ok(()) => {
                            if name == "fit_pv" || name.starts_with("hybrid_pv_") {
                                pv.push((reading.value, reading.observed_at));
                            }
                            entry = serde_json::json!({"status": "valid", "entity_id": id, "reading": reading});
                        }
                        Err(reason) => entry["status"] = reason.into(),
                    },
                    Err(error) => {
                        entry["status"] =
                            crate::home_assistant::reading_error_status(error.as_ref()).into()
                    }
                }
            }
        }
        result.insert(name.into(), entry);
    }
    // Missing, duplicate or invalid sources cannot silently become zero generation.
    let confirmed = state.collection.pv_confirmed;
    let total = solar_total(&pv, confirmed);
    result.insert("solar_total_w".into(), serde_json::json!(total));
    result.insert(
        "solar_total_basis".into(),
        serde_json::json!("estimate: FIT AC output plus hybrid DC PV inputs"),
    );
    result.insert("pv_mappings_confirmed".into(), serde_json::json!(confirmed));
    result.insert(
        "solar_forecast".into(),
        serde_json::json!({"endpoint":"/api/forecasts"}),
    );
    Json(serde_json::Value::Object(result))
}

#[cfg(test)]
mod energy_tests {
    use super::*;
    #[test]
    fn incomplete_unconfirmed_and_skewed_solar_is_not_zero() {
        let now = chrono::Utc::now();
        let values = [(1.0, now), (2.0, now), (3.0, now)];
        assert_eq!(solar_total(&values, true), Some(6.0));
        assert_eq!(solar_total(&values[..2], true), None);
        assert_eq!(solar_total(&values, false), None);
        let mut skewed = values;
        skewed[2].1 = now - chrono::Duration::seconds(31);
        assert_eq!(solar_total(&skewed, true), None);
    }
    #[test]
    fn units_and_negative_prices() {
        let now = chrono::Utc::now();
        let mut r = crate::home_assistant::Reading {
            value: 1.5,
            unit: Some("kW".into()),
            last_updated: now,
            last_changed: now,
            observed_at: now,
        };
        normalize(&mut r, "W", false).unwrap();
        assert_eq!(r.value, 1500.0);
        r.value = -5.0;
        r.unit = Some("p/kWh".into());
        normalize(&mut r, "GBP/kWh", true).unwrap();
        assert_eq!(r.value, -0.05);
        assert!(normalize(&mut r, "W", false).is_err());
        r.unit = Some("W".into());
        assert!(normalize(&mut r, "W", false).is_err());
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct PriceInterval {
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
    value_inc_vat: f64,
}

fn price_intervals(
    attributes: &serde_json::Value,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<serde_json::Value, &'static str> {
    let tariff = attributes
        .get("tariff_code")
        .and_then(|v| v.as_str())
        .ok_or("missing_tariff")?;
    if !tariff.starts_with("E-1R-AGILE-") {
        return Err("not_agile_import");
    }
    let rates: Vec<PriceInterval> =
        serde_json::from_value(attributes.get("rates").cloned().ok_or("missing_rates")?)
            .map_err(|_| "invalid_rates")?;
    if rates.is_empty() {
        return Err("unpublished");
    }
    let mut previous = None;
    for r in &rates {
        if r.end.signed_duration_since(r.start) != chrono::Duration::minutes(30)
            || !r.value_inc_vat.is_finite()
            || previous.is_some_and(|end| end != r.start)
        {
            return Err("invalid_or_incomplete_intervals");
        }
        previous = Some(r.end);
    }
    let current = rates.iter().find(|r| r.start <= now && now < r.end);
    Ok(
        serde_json::json!({"unit": "GBP/kWh", "vat_included": true, "current": current, "intervals": rates,
        "coverage_start": rates.first().map(|r| r.start), "coverage_end": rates.last().map(|r| r.end), "has_current_interval": current.is_some()}),
    )
}

async fn prices(State(state): State<AppState>) -> Json<serde_json::Value> {
    let id = state.collection.rates_entity;
    if id.is_empty() {
        return Json(serde_json::json!({"status": "not_configured"}));
    }
    let now = chrono::Utc::now();
    let result = match state.ha.entity(&id).await {
        Ok(entity) => match price_intervals(&entity.attributes, now) {
            Ok(data) => {
                serde_json::json!({"status": if data["has_current_interval"] == true { "current_available" } else { "no_current_interval" }, "entity_id": id, "fetched_at": now, "last_updated":entity.last_updated, "last_changed":entity.last_changed, "last_reported":entity.last_reported, "source":"Home Assistant Octopus event", "data": data})
            }
            Err(reason) => serde_json::json!({"status": reason}),
        },
        Err(_) => serde_json::json!({"status": "unavailable"}),
    };
    Json(result)
}

#[cfg(test)]
mod price_tests {
    use super::*;
    #[test]
    fn validates_periods_and_allows_negative_prices() {
        let start = chrono::DateTime::parse_from_rfc3339("2026-10-27T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut attributes = serde_json::json!({"tariff_code":"E-1R-AGILE-TEST-A", "rates":[{"start":start,"end":start+chrono::Duration::minutes(30),"value_inc_vat":-0.05}]});
        let data = price_intervals(&attributes, start).unwrap();
        assert_eq!(data["current"]["value_inc_vat"], -0.05);
        assert!(
            price_intervals(&attributes, start + chrono::Duration::minutes(30)).unwrap()["current"]
                .is_null()
        );
        attributes["rates"][0]["end"] = serde_json::json!(start);
        assert!(price_intervals(&attributes, start).is_err());
        attributes["tariff_code"] = "G-1R-TEST".into();
        assert!(price_intervals(&attributes, start).is_err());
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    pub(super) async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (url, task)
    }
    #[tokio::test]
    async fn preserves_endpoints_with_read_only_battery_requests() {
        let mock = Router::new().route("/api/states/{id}", get(|axum::extract::Path(id): axum::extract::Path<String>, headers: axum::http::HeaderMap| async move {
            assert_eq!(headers.get("authorization").unwrap(), "Bearer test-only-token");
            let now = chrono::Utc::now();
            Json(serde_json::json!({"state":if id.ends_with("_soc") {"55"} else {"2"}, "attributes":{"unit_of_measurement":if id.ends_with("_soc") {"%"} else {"kW"}}, "last_changed":now,"last_updated":now,"last_reported":now}))
        }));
        let (ha_url, ha_task) = serve(mock).await;
        let state = AppState {
            ha: HomeAssistantClient::new(ha_url, "test-only-token".into()).unwrap(),
            collection: CollectionConfig {
                sources: Default::default(),
                rates_entity: String::new(),
                pv_confirmed: false,
                forecast_entities: Vec::new(),
                forecast_confirmed: false,
            },
        };
        let (url, task) = serve(router(state)).await;
        let c = reqwest::Client::new();
        for (path, field, expected) in [
            ("health", "status", "healthy"),
            ("version", "version", env!("CARGO_PKG_VERSION")),
        ] {
            let response = c.get(format!("{url}/{path}")).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body: serde_json::Value = response.json().await.unwrap();
            assert_eq!(body[field], expected);
        }
        let response = c.get(format!("{url}/battery")).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(
            body,
            serde_json::json!({"soc_percent":55.0,"charge_power_w":2000.0,"discharge_power_w":2000.0})
        );
        task.abort();
        ha_task.abort();
    }
    #[tokio::test]
    async fn upstream_failure_is_generic_bad_gateway() {
        let mock = Router::new().route(
            "/api/states/{id}",
            get(|| async { StatusCode::UNAUTHORIZED }),
        );
        let (ha_url, ha_task) = serve(mock).await;
        let state = AppState {
            ha: HomeAssistantClient::new(ha_url, "test-only-token".into()).unwrap(),
            collection: CollectionConfig {
                sources: Default::default(),
                rates_entity: String::new(),
                pv_confirmed: false,
                forecast_entities: Vec::new(),
                forecast_confirmed: false,
            },
        };
        let (url, task) = serve(router(state)).await;
        let response = reqwest::get(format!("{url}/battery")).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(!response.text().await.unwrap().contains("test-only-token"));
        task.abort();
        ha_task.abort();
    }
}

// Horizon and physical array are configured explicitly; entity names are never inferred.
const FORECAST_INPUTS: &[(&str, &str, &str)] = &[
    ("fit", "today", "HEMS_FORECAST_FIT_TODAY_ENTITY"),
    ("fit", "tomorrow", "HEMS_FORECAST_FIT_TOMORROW_ENTITY"),
    ("hybrid_1", "today", "HEMS_FORECAST_HYBRID1_TODAY_ENTITY"),
    (
        "hybrid_1",
        "tomorrow",
        "HEMS_FORECAST_HYBRID1_TOMORROW_ENTITY",
    ),
    ("hybrid_2", "today", "HEMS_FORECAST_HYBRID2_TODAY_ENTITY"),
    (
        "hybrid_2",
        "tomorrow",
        "HEMS_FORECAST_HYBRID2_TOMORROW_ENTITY",
    ),
];

fn forecast_value(reading: &mut crate::home_assistant::Reading) -> Result<(), &'static str> {
    match reading.unit.as_deref() {
        Some("Wh") => reading.value /= 1000.0,
        Some("kWh") => (),
        _ => return Err("invalid_unit"),
    }
    if !reading.value.is_finite() || reading.value < 0.0 {
        return Err("invalid_value");
    }
    reading.unit = Some("kWh".into());
    Ok(())
}

async fn forecasts(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mut entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (index, &(array, horizon, _)) in FORECAST_INPUTS.iter().enumerate() {
        let id = state
            .collection
            .forecast_entities
            .get(index)
            .cloned()
            .unwrap_or_default();
        let mut entry = serde_json::json!({"array":array,"horizon":horizon,
            "entity_id":id,"source":"Forecast.Solar via Home Assistant",
            "status":"not_configured","reading":null});
        let status = if id.is_empty() {
            "not_configured"
        } else if !state.collection.forecast_confirmed {
            "mapping_unverified"
        } else if !seen.insert(id.clone()) {
            "duplicate_entity"
        } else {
            // Forecasts update less frequently than power telemetry. Retain the original
            // HA timestamps; a fetch time is never presented as forecast generation time.
            match state.ha.entity(&id).await.and_then(|entity| {
                let reading = crate::home_assistant::validate(&entity, chrono::Utc::now(), 7200)?;
                entry["source_attributes"] = entity.attributes;
                Ok(reading)
            }) {
                Ok(mut reading) => match forecast_value(&mut reading) {
                    Ok(()) => {
                        entry["reading"] = serde_json::to_value(reading).unwrap();
                        "valid"
                    }
                    Err(reason) => reason,
                },
                Err(error) => crate::home_assistant::reading_error_status(error.as_ref()),
            }
        };
        entry["status"] = status.into();
        entries.push(entry);
    }
    Json(
        serde_json::json!({"fetched_at":chrono::Utc::now(),"timezone":"Europe/London",
        "max_age_seconds":7200,"mappings_confirmed":state.collection.forecast_confirmed,
        "forecasts":entries}),
    )
}

#[cfg(test)]
mod forecast_tests {
    use super::*;
    #[test]
    fn energy_units_and_invalid_values() {
        let now = chrono::Utc::now();
        let mut r = crate::home_assistant::Reading {
            value: 3100.0,
            unit: Some("Wh".into()),
            last_updated: now,
            last_changed: now,
            observed_at: now,
        };
        forecast_value(&mut r).unwrap();
        assert_eq!(r.value, 3.1);
        assert_eq!(r.unit.as_deref(), Some("kWh"));
        for value in [-1.0, f64::NAN, f64::INFINITY] {
            r.value = value;
            assert!(forecast_value(&mut r).is_err());
        }
        r.value = 1.0;
        r.unit = Some("W".into());
        assert!(forecast_value(&mut r).is_err());
    }
    #[tokio::test]
    async fn forecast_reads_preserve_source_and_reject_stale_data() {
        let now = chrono::Utc::now();
        let old = now - chrono::Duration::hours(3);
        let mock = Router::new().route("/api/states/{id}", get(move |axum::extract::Path(id): axum::extract::Path<String>| async move {
            let timestamp = if id.ends_with("stale") { old } else { now };
            Json(serde_json::json!({"state":"3100","attributes":{"unit_of_measurement":"Wh","fixture_issue_time":"2026-10-28T00:00:00Z"},
                "last_changed":timestamp,"last_updated":timestamp,"last_reported":timestamp}))
        }));
        let (url, task) = super::http_tests::serve(mock).await;
        let response = forecasts(State(AppState {
            ha: HomeAssistantClient::new(url, "test".into()).unwrap(),
            collection: CollectionConfig {
                sources: Default::default(),
                rates_entity: String::new(),
                pv_confirmed: false,
                forecast_entities: vec![
                    "sensor.fixture_fresh".into(),
                    "sensor.fixture_stale".into(),
                ],
                forecast_confirmed: true,
            },
        }))
        .await
        .0;
        assert_eq!(response["forecasts"][0]["status"], "valid");
        assert_eq!(response["forecasts"][0]["reading"]["value"], 3.1);
        assert_eq!(
            response["forecasts"][0]["reading"]["last_updated"],
            serde_json::json!(now)
        );
        assert_eq!(
            response["forecasts"][0]["source_attributes"]["fixture_issue_time"],
            "2026-10-28T00:00:00Z"
        );
        assert_eq!(response["forecasts"][1]["status"], "stale");
        assert!(response["forecasts"][1]["reading"].is_null());
        task.abort();
    }

    #[tokio::test]
    async fn unconfirmed_and_duplicate_mappings_do_not_read_twice() {
        let mut state = AppState {
            ha: HomeAssistantClient::new("http://127.0.0.1:1".into(), "test".into()).unwrap(),
            collection: CollectionConfig {
                sources: Default::default(),
                rates_entity: String::new(),
                pv_confirmed: false,
                forecast_entities: vec!["sensor.test_fixture".into(); 6],
                forecast_confirmed: false,
            },
        };
        let response = forecasts(State(state.clone())).await.0;
        assert!(
            response["forecasts"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["status"] == "mapping_unverified")
        );
        state.collection.forecast_confirmed = true;
        let response = forecasts(State(state)).await.0;
        assert_eq!(response["forecasts"][0]["status"], "upstream_error");
        assert!(
            response["forecasts"].as_array().unwrap()[1..]
                .iter()
                .all(|e| e["status"] == "duplicate_entity")
        );
    }

    #[tokio::test]
    async fn missing_forecasts_remain_explicit_and_separate() {
        let response = forecasts(State(AppState {
            ha: HomeAssistantClient::new("http://127.0.0.1:1".into(), "test".into()).unwrap(),
            collection: CollectionConfig {
                sources: Default::default(),
                rates_entity: String::new(),
                pv_confirmed: false,
                forecast_entities: Vec::new(),
                forecast_confirmed: false,
            },
        }))
        .await
        .0;
        let entries = response["forecasts"].as_array().unwrap();
        assert_eq!(entries.len(), 6);
        for e in entries {
            assert_eq!(e["status"], "not_configured");
            assert!(e["reading"].is_null());
        }
    }
}
