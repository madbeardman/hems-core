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
    freshness: serde_json::Value,
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
    let snapshot = state.ha.battery_snapshot().await.map_err(|_| {
        (
            StatusCode::BAD_GATEWAY,
            Json(ApiError {
                error: "Unable to retrieve battery data from Home Assistant",
            }),
        )
    })?;

    let [soc, charge, discharge] = snapshot.values;
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
        freshness: snapshot.freshness,
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
    next_day_rates_entity: String,
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
        let next_day_rates_entity =
            std::env::var("HEMS_AGILE_NEXT_DAY_RATES_ENTITY").unwrap_or_default();
        if !next_day_rates_entity.is_empty()
            && (!next_day_rates_entity.starts_with("event.") || !valid_id(&next_day_rates_entity))
        {
            return Err("Invalid HEMS_AGILE_NEXT_DAY_RATES_ENTITY".into());
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
            next_day_rates_entity,
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
    ("grid_net", "HEMS_GRID_NET_ENTITY", "W"),
    ("hybrid_ac", "HEMS_HYBRID_AC_ENTITY", "W"),
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

const SOLAR_MAX_SKEW_SECONDS: i64 = 120;

fn solar_total(pv: &[(f64, chrono::DateTime<chrono::Utc>)], confirmed: bool) -> Option<f64> {
    if !confirmed || pv.len() != 3 {
        return None;
    }
    let oldest = pv.iter().map(|(_, time)| time).min()?;
    let newest = pv.iter().map(|(_, time)| time).max()?;
    if *newest - *oldest > chrono::Duration::seconds(SOLAR_MAX_SKEW_SECONDS) {
        return None;
    }
    let sum: f64 = pv.iter().map(|(value, _)| value).sum();
    if sum.is_finite() { Some(sum) } else { None }
}

fn derive_stale_grid_directions(result: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(net) = result
        .get("grid_net")
        .filter(|e| e["status"] == "valid")
        .cloned()
    else {
        return;
    };
    let Some(value) = net["reading"]["value"].as_f64().filter(|v| v.is_finite()) else {
        return;
    };
    for (name, derived, formula) in [
        ("grid_import", value.max(0.0), "max(grid_net, 0)"),
        ("grid_export", (-value).max(0.0), "max(-grid_net, 0)"),
    ] {
        let Some(original) = result.get(name).filter(|e| e["status"] == "stale").cloned() else {
            continue;
        };
        let mut reading = net["reading"].clone();
        reading["value"] = serde_json::json!(derived);
        result.insert(
            name.into(),
            serde_json::json!({
                "status":"derived", "entity_id":net["entity_id"], "reading":reading,
                "derived_from":"grid_net", "formula":formula, "reported_sensor":original,
            }),
        );
    }
}

// User policy: unavailable solar contributes numeric zero, with explicit
// quality metadata. This is an assumption, never a replacement measurement.
fn apply_solar_zero_policy(result: &mut serde_json::Map<String, serde_json::Value>) -> usize {
    let mut assumed = 0;
    for name in ["fit_pv", "hybrid_pv_1", "hybrid_pv_2"] {
        if let Some(entry) = result.get_mut(name) {
            if entry["status"] != "valid" {
                entry["source_status"] = entry["status"].clone();
                entry["status"] = "assumed_zero".into();
                entry["value"] = serde_json::json!(0.0);
                entry["reading"] = serde_json::json!({"value":0.0,"unit":"W"});
                entry["assumption"] =
                    "unavailable solar treated as zero by user policy; not a fresh measurement"
                        .into();
                assumed += 1;
            }
        }
    }
    assumed
}

fn solar_public_total(
    pv: &[(f64, chrono::DateTime<chrono::Utc>)],
    confirmed: bool,
    assumed: usize,
) -> (f64, &'static str) {
    if !confirmed {
        return (0.0, "mapping_unverified_assumed_zero");
    }
    if assumed == 0 {
        return match solar_total(pv, true) {
            Some(value) => (value, "valid_estimate"),
            None => (0.0, "unavailable_total_assumed_zero"),
        };
    }
    if pv.len() + assumed != 3 {
        return (0.0, "unavailable_total_assumed_zero");
    }
    if let Some((oldest, newest)) = pv
        .iter()
        .map(|(_, t)| t)
        .min()
        .zip(pv.iter().map(|(_, t)| t).max())
    {
        if *newest - *oldest > chrono::Duration::seconds(SOLAR_MAX_SKEW_SECONDS) {
            return (0.0, "timestamp_skew_assumed_zero");
        }
    }
    let sum: f64 = pv.iter().map(|(v, _)| v).sum();
    if !sum.is_finite() {
        return (0.0, "invalid_total_assumed_zero");
    }
    (
        if sum == 0.0 { 0.0 } else { sum },
        "estimated_with_assumed_zero",
    )
}

const HOUSEHOLD_AC_INPUTS: &[(&str, &str)] = &[
    ("grid_net", "sensor.solis_s6_eh1p_grid_power_net"),
    ("fit_pv", "sensor.solis_s6_gr1p_active_power"),
    ("hybrid_ac", "sensor.solis_s6_eh1p_active_power"),
];

fn household_balance(result: &serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
    let mut entry = serde_json::json!({"status":"incomplete_sources","reading":null,
        "formula":"grid_net + fit_pv + hybrid_ac", "basis":"AC power balance estimate",
        "max_skew_seconds":120,"sources":{}});
    let mut sum = 0.0;
    let mut assumed_fit = false;
    let mut times = Vec::new();
    for &(name, _) in HOUSEHOLD_AC_INPUTS {
        entry["sources"][name] = result.get(name).cloned().unwrap_or(serde_json::Value::Null);
    }
    for &(name, expected_id) in HOUSEHOLD_AC_INPUTS {
        let Some(source) = result.get(name) else {
            return entry;
        };
        if source["entity_id"] != expected_id {
            entry["status"] = "mapping_unverified".into();
            return entry;
        }
        if name == "fit_pv" && source["status"] == "assumed_zero" {
            assumed_fit = true;
            continue; // Zero assumption has no observation timestamp to invent.
        }
        if source["status"] != "valid" {
            return entry;
        }
        let reading = &source["reading"];
        let Some(value) = reading["value"].as_f64().filter(|v| v.is_finite()) else {
            return entry;
        };
        if reading["unit"] != "W" {
            return entry;
        }
        let Ok(time) =
            serde_json::from_value::<chrono::DateTime<chrono::Utc>>(reading["observed_at"].clone())
        else {
            return entry;
        };
        sum += value;
        times.push(time);
    }
    let oldest = *times.iter().min().unwrap();
    let newest = *times.iter().max().unwrap();
    let span = (newest - oldest).num_milliseconds() as f64 / 1000.0;
    entry["observation_span_seconds"] = serde_json::json!(span);
    if newest - oldest > chrono::Duration::seconds(120) {
        entry["status"] = "timestamp_skew".into();
        return entry;
    }
    if !sum.is_finite() || sum < 0.0 {
        entry["status"] = "invalid_balance".into();
        return entry;
    }
    entry["status"] = if assumed_fit {
        "estimated_with_assumed_zero"
    } else {
        "derived"
    }
    .into();
    entry["reading"] =
        serde_json::json!({"value":sum,"unit":"W","observed_from":oldest,"observed_to":newest});
    entry
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
        let mut entry =
            serde_json::json!({"status": "not_configured", "entity_id": id, "value": null});
        if !id.is_empty() {
            if !seen.insert(id.clone()) {
                entry["status"] = "duplicate_entity".into();
            } else {
                match state.ha.reading(&id).await {
                    Ok(mut reading) => match normalize(
                        &mut reading,
                        unit,
                        matches!(name, "grid_net" | "hybrid_ac"),
                    ) {
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
        if name == "grid_net" {
            entry["sign_convention"] = "positive_import_negative_export".into();
        }
        if name == "hybrid_ac" {
            entry["basis"] = "inverter AC active power; separate from DC PV inputs".into();
            entry["sign_convention"] = "positive_output_negative_input".into();
            entry["battery_discharge_included"] = true.into();
            entry["ac_charging_sign_confirmed"] = true.into();
        }
        if name == "household" && !id.is_empty() {
            entry["basis"] = "configured household-load sensor via Home Assistant".into();
        }
        result.insert(name.into(), entry);
    }
    let assumed_solar_inputs = apply_solar_zero_policy(&mut result);
    derive_stale_grid_directions(&mut result);
    let calculated_household = household_balance(&result);
    result.insert("household_ac_balance".into(), calculated_household.clone());
    // Prefer the configured meter-backed household source. Keep the independent
    // AC balance available for comparison even when that source is unavailable.
    if state
        .collection
        .sources
        .get("household")
        .is_none_or(|id| id.is_empty())
    {
        result.insert("household".into(), calculated_household);
    }
    // Numeric zero fallback is explicit in quality metadata, not a fresh reading.
    let confirmed = state.collection.pv_confirmed;
    let (total, total_status) = solar_public_total(&pv, confirmed, assumed_solar_inputs);
    result.insert(
        "solar_assumed_zero_inputs".into(),
        serde_json::json!(assumed_solar_inputs),
    );
    let span = pv
        .iter()
        .map(|(_, t)| *t)
        .max()
        .zip(pv.iter().map(|(_, t)| *t).min())
        .map(|(newest, oldest)| (newest - oldest).num_milliseconds() as f64 / 1000.0);
    result.insert("solar_total_status".into(), serde_json::json!(total_status));
    result.insert(
        "solar_total_max_skew_seconds".into(),
        serde_json::json!(SOLAR_MAX_SKEW_SECONDS),
    );
    result.insert(
        "solar_total_observation_span_seconds".into(),
        serde_json::json!(span),
    );
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
        skewed[2].1 = now - chrono::Duration::seconds(121);
        assert_eq!(solar_total(&skewed, true), None);
    }
    #[test]
    fn asynchronous_solar_updates_have_an_explicit_skew_limit() {
        let now = chrono::Utc::now();
        let mut pv = [
            (40.0, now),
            (97.0, now - chrono::Duration::seconds(20)),
            (127.0, now - chrono::Duration::seconds(50)),
        ];
        assert_eq!(solar_total(&pv, true), Some(264.0));
        pv[2].1 = now - chrono::Duration::seconds(120);
        assert_eq!(solar_total(&pv, true), Some(264.0));
        pv[2].1 = now - chrono::Duration::seconds(121);
        assert_eq!(solar_total(&pv, true), None);
    }

    #[test]
    fn grid_derivation_requires_fresh_net_and_preserves_provenance() {
        for net in [-193.0_f64, 250.0, 0.0] {
            let timestamp = "2026-10-28T14:41:35Z";
            let mut entries = serde_json::Map::new();
            entries.insert(
                "grid_net".into(),
                serde_json::json!({"status":"valid","entity_id":"sensor.fixture_net",
                "reading":{"value":net,"unit":"W","observed_at":timestamp}}),
            );
            for name in ["grid_import", "grid_export"] {
                entries.insert(name.into(),serde_json::json!({"status":"stale","entity_id":"sensor.fixture_direction","value":null}));
            }
            let original = entries.clone();
            derive_stale_grid_directions(&mut entries);
            assert_eq!(entries["grid_import"]["reading"]["value"], net.max(0.0));
            assert_eq!(entries["grid_export"]["reading"]["value"], (-net).max(0.0));
            assert_eq!(entries["grid_import"]["status"], "derived");
            assert_eq!(entries["grid_import"]["reading"]["observed_at"], timestamp);
            assert_eq!(
                entries["grid_import"]["reported_sensor"],
                original["grid_import"]
            );
            for bad in ["stale", "invalid_state", "not_configured"] {
                let mut rejected = original.clone();
                rejected.get_mut("grid_net").unwrap()["status"] = bad.into();
                let before = rejected.clone();
                derive_stale_grid_directions(&mut rejected);
                assert_eq!(rejected, before);
            }
            for status in [
                "valid",
                "invalid_value",
                "duplicate_entity",
                "not_configured",
            ] {
                let mut preserved = original.clone();
                preserved.get_mut("grid_import").unwrap()["status"] = status.into();
                let before = preserved["grid_import"].clone();
                derive_stale_grid_directions(&mut preserved);
                assert_eq!(preserved["grid_import"], before);
            }
        }
    }

    #[test]
    fn household_ac_balance_handles_export_discharge_and_forced_charge() {
        let now = chrono::Utc::now();
        let make = |grid: f64, fit: f64, hybrid: f64| {
            let mut map = serde_json::Map::new();
            for ((name, id), value) in HOUSEHOLD_AC_INPUTS.iter().zip([grid, fit, hybrid]) {
                map.insert(
                    (*name).into(),
                    serde_json::json!({"status":"valid","entity_id":id,
                    "reading":{"value":value,"unit":"W","observed_at":now}}),
                );
            }
            // Neither the DC array values nor separate battery power may enter the balance.
            map.insert(
                "battery_discharge".into(),
                serde_json::json!({"value":392.0}),
            );
            map.insert("hybrid_pv_1".into(), serde_json::json!({"value":999.0}));
            map
        };
        for (grid, fit, hybrid, expected) in [
            (0.0, 0.0, 390.0, 390.0),
            (1440.0, 0.0, -1160.0, 280.0),
            (-500.0, 1000.0, 300.0, 800.0),
            (0.0, 0.0, 0.0, 0.0),
        ] {
            let data = household_balance(&make(grid, fit, hybrid));
            assert_eq!(data["status"], "derived");
            assert_eq!(data["reading"]["value"], expected);
        }
        let valid = make(1440.0, 0.0, -1160.0);
        for name in ["fit_pv", "grid_net", "hybrid_ac"] {
            let mut stale = valid.clone();
            stale.get_mut(name).unwrap()["status"] = "stale".into();
            assert!(household_balance(&stale)["reading"].is_null());
            let mut missing = valid.clone();
            missing.remove(name);
            assert!(household_balance(&missing)["reading"].is_null());
        }
        let mut skewed = valid.clone();
        skewed.get_mut("fit_pv").unwrap()["reading"]["observed_at"] =
            serde_json::json!(now - chrono::Duration::seconds(121));
        assert_eq!(household_balance(&skewed)["status"], "timestamp_skew");
        let mut unverified = valid.clone();
        unverified.get_mut("hybrid_ac").unwrap()["entity_id"] = "sensor.other".into();
        assert_eq!(
            household_balance(&unverified)["status"],
            "mapping_unverified"
        );
        assert_eq!(
            household_balance(&make(-1000.0, 0.0, 390.0))["status"],
            "invalid_balance"
        );
    }

    #[test]
    fn unavailable_solar_is_numeric_zero_with_quality_and_household_assumption() {
        let now = chrono::Utc::now();
        let mut entries = serde_json::Map::new();
        for (name, id, value) in [
            ("grid_net", HOUSEHOLD_AC_INPUTS[0].1, -60.0),
            ("hybrid_ac", HOUSEHOLD_AC_INPUTS[2].1, 460.0),
        ] {
            entries.insert(
                name.into(),
                serde_json::json!({"status":"valid","entity_id":id,
                "reading":{"value":value,"unit":"W","observed_at":now}}),
            );
        }
        for name in ["fit_pv", "hybrid_pv_1", "hybrid_pv_2"] {
            entries.insert(name.into(),serde_json::json!({"entity_id":if name=="fit_pv" {HOUSEHOLD_AC_INPUTS[1].1} else {"sensor.fixture"},"status":"stale","value":null}));
        }
        assert_eq!(apply_solar_zero_policy(&mut entries), 3);
        assert_eq!(entries["fit_pv"]["value"], 0.0);
        assert_eq!(entries["fit_pv"]["reading"]["value"], 0.0);
        assert_eq!(entries["fit_pv"]["source_status"], "stale");
        assert!(entries["fit_pv"]["reading"].get("observed_at").is_none());
        let household = household_balance(&entries);
        assert_eq!(household["status"], "estimated_with_assumed_zero");
        assert_eq!(household["reading"]["value"], 400.0);
        entries.get_mut("grid_net").unwrap()["status"] = "stale".into();
        assert!(household_balance(&entries)["reading"].is_null());
        assert_eq!(
            solar_public_total(&[], true, 3),
            (0.0, "estimated_with_assumed_zero")
        );
        assert_eq!(
            solar_public_total(&[(50.0, now)], true, 2),
            (50.0, "estimated_with_assumed_zero")
        );
        assert_eq!(
            solar_public_total(
                &[(50.0, now), (10.0, now - chrono::Duration::seconds(121))],
                true,
                1
            ),
            (0.0, "timestamp_skew_assumed_zero")
        );
        assert_eq!(
            solar_public_total(&[], false, 3),
            (0.0, "mapping_unverified_assumed_zero")
        );
    }

    #[test]
    fn signed_grid_net_allows_export_but_directional_power_does_not() {
        let now = chrono::Utc::now();
        let mut r = crate::home_assistant::Reading {
            value: -0.791,
            unit: Some("kW".into()),
            last_updated: now,
            last_changed: now,
            observed_at: now,
        };
        normalize(&mut r, "W", true).unwrap();
        assert_eq!(r.value, -791.0);
        assert!(normalize(&mut r, "W", false).is_err());
        r.value = 791.0;
        assert!(normalize(&mut r, "W", false).is_ok());
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    is_capped: Option<bool>,
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
        serde_json::json!({"unit": "GBP/kWh", "vat_included": true, "tariff_code":tariff, "interval_count":rates.len(), "current": current, "intervals": rates,
        "coverage_start": rates.first().map(|r| r.start), "coverage_end": rates.last().map(|r| r.end), "has_current_interval": current.is_some()}),
    )
}

async fn price_feed(ha: &HomeAssistantClient, id: &str, next_day: bool) -> serde_json::Value {
    if id.is_empty() {
        return serde_json::json!({"status":"not_configured"});
    }
    match ha.entity(id).await {
        Ok(entity) => {
            let now = chrono::Utc::now();
            match price_intervals(&entity.attributes, now) {
                Ok(data) => {
                    let status = if next_day {
                        if data["coverage_end"]
                            .as_str()
                            .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok())
                            .is_some_and(|end| end > now)
                        {
                            "schedule_available"
                        } else {
                            "expired_schedule"
                        }
                    } else if data["has_current_interval"] == true {
                        "current_available"
                    } else {
                        "no_current_interval"
                    };
                    serde_json::json!({"status":status,"entity_id":id,"fetched_at":now,
                        "last_updated":entity.last_updated,"last_changed":entity.last_changed,"last_reported":entity.last_reported,
                        "source":"Home Assistant Octopus event","data":data})
                }
                Err(reason) => serde_json::json!({"status":reason,"entity_id":id,"fetched_at":now,
                    "last_updated":entity.last_updated,"last_changed":entity.last_changed,"last_reported":entity.last_reported}),
            }
        }
        Err(_) => serde_json::json!({"status":"unavailable","entity_id":id}),
    }
}

async fn prices(State(state): State<AppState>) -> Json<serde_json::Value> {
    let current_id = &state.collection.rates_entity;
    let next_id = &state.collection.next_day_rates_entity;
    let mut current = price_feed(&state.ha, current_id, false).await;
    let next = if !next_id.is_empty() && next_id == current_id {
        serde_json::json!({"status":"duplicate_entity"})
    } else {
        price_feed(&state.ha, next_id, true).await
    };
    current["next_day"] = next;
    Json(current)
}

#[cfg(test)]
mod price_tests {
    use super::*;
    #[test]
    fn partial_day_coverage_is_explicit_and_never_extended() {
        let start = chrono::DateTime::parse_from_rfc3339("2026-10-28T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let rates: Vec<_> = (0..46)
            .map(|n| {
                serde_json::json!({
                    "start":start+chrono::Duration::minutes(n*30),
                    "end":start+chrono::Duration::minutes((n+1)*30),
                    "value_inc_vat":0.2806,"is_capped":false,
                })
            })
            .collect();
        let attrs = serde_json::json!({"tariff_code":"E-1R-AGILE-TEST-A","rates":rates});
        let data = price_intervals(&attrs, start).unwrap();
        assert_eq!(data["interval_count"], 46);
        assert_eq!(
            data["coverage_end"],
            serde_json::json!(start + chrono::Duration::hours(23))
        );
        assert_eq!(data["current"]["value_inc_vat"], 0.2806); // GBP, no extra /100 conversion.
        assert_eq!(data["current"]["is_capped"], false);
        assert!(
            price_intervals(&attrs, start + chrono::Duration::hours(23)).unwrap()["current"]
                .is_null()
        );
        assert!(
            price_intervals(&attrs, start - chrono::Duration::seconds(1)).unwrap()["current"]
                .is_null()
        );
        let mut gap = attrs.clone();
        gap["rates"].as_array_mut().unwrap().remove(1);
        assert!(price_intervals(&gap, start).is_err());
        let mut duplicate = attrs.clone();
        let first = duplicate["rates"][0].clone();
        duplicate["rates"][1] = first;
        assert!(price_intervals(&duplicate, start).is_err());
        assert_eq!(
            price_intervals(
                &serde_json::json!({"tariff_code":"E-1R-AGILE-TEST-A","rates":[]}),
                start
            )
            .unwrap_err(),
            "unpublished"
        );
    }

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
                next_day_rates_entity: String::new(),
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
            serde_json::json!({"soc_percent":body["soc_percent"],"charge_power_w":body["charge_power_w"],"discharge_power_w":body["discharge_power_w"]}),
            serde_json::json!({"soc_percent":55.0,"charge_power_w":2000.0,"discharge_power_w":2000.0})
        );
        assert_eq!(body["freshness"]["all_sources_recent"], true);
        assert_eq!(
            body["freshness"]["sources"]["soc_percent"]["status"],
            "fresh"
        );
        assert!(body["freshness"]["sources"]["discharge_power_w"]["last_reported"].is_string());
        task.abort();
        ha_task.abort();
    }
    #[tokio::test]
    async fn grid_readings_keep_net_separate_from_solar() {
        let mock = Router::new().route(
            "/api/states/{id}",
            get(
                |axum::extract::Path(id): axum::extract::Path<String>| async move {
                    let now = chrono::Utc::now();
                    let value = match id.as_str() {
                        "sensor.fixture_import" => "0",
                        "sensor.fixture_export" => "791",
                        _ => "-791",
                    };
                    Json(
                        serde_json::json!({"state":value,"attributes":{"unit_of_measurement":"W"},
                "last_updated":now,"last_changed":now,"last_reported":now}),
                    )
                },
            ),
        );
        let (url, task) = serve(mock).await;
        let state = AppState {
            ha: HomeAssistantClient::new(url, "test".into()).unwrap(),
            collection: CollectionConfig {
                sources: [
                    ("grid_import", "sensor.fixture_import".into()),
                    ("grid_export", "sensor.fixture_export".into()),
                    ("grid_net", "sensor.fixture_net".into()),
                    ("hybrid_ac", "sensor.fixture_ac".into()),
                ]
                .into_iter()
                .collect(),
                rates_entity: String::new(),
                next_day_rates_entity: String::new(),
                pv_confirmed: false,
                forecast_entities: Vec::new(),
                forecast_confirmed: false,
            },
        };
        let data = energy(State(state)).await.0;
        assert_eq!(data["grid_import"]["reading"]["value"], 0.0);
        assert_eq!(data["grid_export"]["reading"]["value"], 791.0);
        assert_eq!(data["grid_net"]["reading"]["value"], -791.0);
        assert_eq!(
            data["grid_net"]["sign_convention"],
            "positive_import_negative_export"
        );
        assert_eq!(data["hybrid_ac"]["reading"]["value"], -791.0);
        assert_eq!(
            data["hybrid_ac"]["sign_convention"],
            "positive_output_negative_input"
        );
        assert_eq!(data["household"]["status"], "mapping_unverified");
        assert_eq!(data["solar_total_w"], 0.0);
        task.abort();
    }

    #[tokio::test]
    async fn price_feeds_keep_current_and_future_schedules_independent() {
        let now = chrono::Utc::now();
        let mock=Router::new().route("/api/states/{id}",get(move |axum::extract::Path(id):axum::extract::Path<String>| async move {
            let start=if id.ends_with("next") {now+chrono::Duration::days(1)} else {now-chrono::Duration::minutes(1)};
            let rates=if id.ends_with("empty") {vec![]} else {vec![serde_json::json!({"start":start,"end":start+chrono::Duration::minutes(30),"value_inc_vat":-0.05,"is_capped":false})]};
            // Old event reporting timestamps do not invalidate absolute schedules.
            let old=now-chrono::Duration::days(1);
            Json(serde_json::json!({"state":"event","last_updated":old,"last_changed":old,
                "attributes":{"tariff_code":"E-1R-AGILE-TEST-A","rates":rates}}))
        }));
        let (url, task) = serve(mock).await;
        let mut state = AppState {
            ha: HomeAssistantClient::new(url, "test".into()).unwrap(),
            collection: CollectionConfig {
                sources: Default::default(),
                rates_entity: "event.fixture_current".into(),
                next_day_rates_entity: "event.fixture_next".into(),
                pv_confirmed: false,
                forecast_entities: Vec::new(),
                forecast_confirmed: false,
            },
        };
        let result = prices(State(state.clone())).await.0;
        assert_eq!(result["status"], "current_available");
        assert_eq!(result["data"]["current"]["value_inc_vat"], -0.05);
        assert_eq!(result["next_day"]["status"], "schedule_available");
        assert!(result["next_day"]["data"]["current"].is_null());
        assert_eq!(result["next_day"]["data"]["interval_count"], 1);
        state.collection.next_day_rates_entity = "event.fixture_empty".into();
        let result = prices(State(state.clone())).await.0;
        assert_eq!(result["status"], "current_available");
        assert_eq!(result["next_day"]["status"], "unpublished");
        state.collection.next_day_rates_entity = state.collection.rates_entity.clone();
        assert_eq!(
            prices(State(state)).await.0["next_day"]["status"],
            "duplicate_entity"
        );
        task.abort();
    }

    #[tokio::test]
    async fn configured_household_sensor_is_primary_and_ac_balance_stays_separate() {
        let now = chrono::Utc::now();
        let mock=Router::new().route("/api/states/{id}",get(move |axum::extract::Path(id):axum::extract::Path<String>|async move {
            if id=="sensor.fixture_unavailable" { return (StatusCode::SERVICE_UNAVAILABLE,Json(serde_json::json!({}))); }
            let value=match id.as_str() {
                "sensor.solis_s6_eh1p_grid_power_net"=>"-78",
                "sensor.solis_s6_gr1p_active_power"=>"20",
                "sensor.solis_s6_eh1p_active_power"=>"250",
                _=>"192",
            };
            (StatusCode::OK,Json(serde_json::json!({"state":value,"attributes":{"unit_of_measurement":"W"},
                "last_changed":now,"last_updated":now,"last_reported":now})))
        }));
        let (url, task) = serve(mock).await;
        let mut sources: std::collections::HashMap<_, _> = HOUSEHOLD_AC_INPUTS
            .iter()
            .map(|(name, id)| (*name, (*id).to_string()))
            .collect();
        sources.insert(
            "household",
            "sensor.solis_s6_eh1p_household_load_power".into(),
        );
        let mut state = AppState {
            ha: HomeAssistantClient::new(url, "test".into()).unwrap(),
            collection: CollectionConfig {
                sources,
                rates_entity: String::new(),
                next_day_rates_entity: String::new(),
                pv_confirmed: false,
                forecast_entities: Vec::new(),
                forecast_confirmed: false,
            },
        };
        let data = energy(State(state.clone())).await.0;
        assert_eq!(data["household"]["status"], "valid");
        assert_eq!(data["household"]["reading"]["value"], 192.0);
        assert_eq!(data["household_ac_balance"]["status"], "derived");
        assert_eq!(data["household_ac_balance"]["reading"]["value"], 192.0);
        state
            .collection
            .sources
            .insert("household", "sensor.fixture_unavailable".into());
        let data = energy(State(state)).await.0;
        assert_eq!(data["household"]["status"], "upstream_error");
        assert_eq!(data["household_ac_balance"]["reading"]["value"], 192.0);
        task.abort();
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
                next_day_rates_entity: String::new(),
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

// Two-hour expected reporting cadence plus a 15-minute scheduling grace period.
const FORECAST_UPDATE_INTERVAL_SECONDS: i64 = 7200;
const FORECAST_GRACE_SECONDS: i64 = 900;
const FORECAST_MAX_AGE_SECONDS: i64 = FORECAST_UPDATE_INTERVAL_SECONDS + FORECAST_GRACE_SECONDS;

fn forecast_orientation(array: &str) -> &'static str {
    match array {
        "fit" => "SW",
        "hybrid_1" => "NW",
        "hybrid_2" => "W",
        _ => "unknown",
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
            "entity_id":id,"orientation":forecast_orientation(array),"source":"Forecast.Solar via Home Assistant",
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
                // Preserve provenance even if validation rejects the value. These are
                // HA state timestamps, not a provider forecast issuance timestamp.
                entry["source_attributes"] = entity.attributes.clone();
                entry["source_timestamps"] = serde_json::json!({
                    "last_changed":entity.last_changed,"last_updated":entity.last_updated,
                    "last_reported":entity.last_reported,
                    "observed_at":entity.last_reported.unwrap_or(entity.last_updated),
                });
                crate::home_assistant::validate(
                    &entity,
                    chrono::Utc::now(),
                    FORECAST_MAX_AGE_SECONDS,
                )
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
        "expected_update_interval_seconds":FORECAST_UPDATE_INTERVAL_SECONDS,
        "freshness_grace_seconds":FORECAST_GRACE_SECONDS,
        "max_age_seconds":FORECAST_MAX_AGE_SECONDS,"mappings_confirmed":state.collection.forecast_confirmed,
        "forecasts":entries}),
    )
}

#[cfg(test)]
mod forecast_tests {
    use super::*;
    #[test]
    fn forecast_cadence_grace_and_array_directions() {
        let now = chrono::Utc::now();
        let timestamp = now - chrono::Duration::seconds(7200 + 300);
        let entity: crate::home_assistant::EntityState =
            serde_json::from_value(serde_json::json!({
                "state":"3.035","attributes":{"unit_of_measurement":"kWh"},
                "last_updated":timestamp,"last_changed":timestamp,"last_reported":timestamp,
            }))
            .unwrap();
        assert!(crate::home_assistant::validate(&entity, now, FORECAST_MAX_AGE_SECONDS).is_ok());
        assert!(
            crate::home_assistant::validate(
                &entity,
                timestamp + chrono::Duration::seconds(8100),
                FORECAST_MAX_AGE_SECONDS
            )
            .is_ok()
        );
        assert!(
            crate::home_assistant::validate(
                &entity,
                timestamp + chrono::Duration::seconds(8101),
                FORECAST_MAX_AGE_SECONDS
            )
            .is_err()
        );
        assert_eq!(forecast_orientation("fit"), "SW");
        assert_eq!(forecast_orientation("hybrid_1"), "NW");
        assert_eq!(forecast_orientation("hybrid_2"), "W");
    }

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
                next_day_rates_entity: String::new(),
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
        assert_eq!(response["forecasts"][0]["orientation"], "SW");
        assert_eq!(
            response["forecasts"][1]["source_timestamps"]["last_updated"],
            serde_json::json!(old)
        );
        assert_eq!(response["max_age_seconds"], 8100);
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
                next_day_rates_entity: String::new(),
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
                next_day_rates_entity: String::new(),
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
