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
    let (soc, charge, discharge) = tokio::try_join!(
        state
            .ha
            .get_numeric_state("sensor.solis_s6_eh1p_battery_soc"),
        state
            .ha
            .get_numeric_state("sensor.solis_s6_eh1p_battery_charge_power"),
        state
            .ha
            .get_numeric_state("sensor.solis_s6_eh1p_battery_discharge_power"),
    )
    .map_err(|_| {
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
        .with_state(state)
}

#[derive(Clone)]
pub struct CollectionConfig {
    sources: std::collections::HashMap<&'static str, String>,
    rates_entity: String,
    pv_confirmed: bool,
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
        Ok(Self {
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
        serde_json::json!({"status":"mapping_unverified", "value":null}),
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
                serde_json::json!({"status": if data["has_current_interval"] == true { "current_available" } else { "no_current_interval" }, "entity_id": id, "fetched_at": now, "last_updated":entity.last_updated, "data": data})
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
    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
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
