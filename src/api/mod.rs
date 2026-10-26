use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde::Serialize;

use crate::home_assistant::HomeAssistantClient;

#[derive(Clone)]
pub struct AppState {
    pub ha: HomeAssistantClient,
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
        .with_state(state)
}
