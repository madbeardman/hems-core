use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

type ReadError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Deserialize)]
pub struct EntityState {
    state: String,
    pub last_updated: chrono::DateTime<chrono::Utc>,
    pub last_changed: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub last_reported: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub attributes: serde_json::Value,
}

#[derive(Clone)]
pub struct HomeAssistantClient {
    client: Client,
    base_url: String,
    token: String,
}

impl HomeAssistantClient {
    pub fn new(base_url: String, token: String) -> Result<Self, reqwest::Error> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
        })
    }

    pub async fn history_battery(&self) -> serde_json::Value {
        let (soc, charge, discharge) = tokio::join!(
            self.entity(BATTERY_IDS[0]),
            self.entity(BATTERY_IDS[1]),
            self.entity(BATTERY_IDS[2]),
        );
        let now = chrono::Utc::now();
        let mut payload = serde_json::json!({
            "freshness":{"fetched_at":now,"sources":{},"max_age_seconds":300,"future_tolerance_seconds":30,
            "timestamp_basis":"Home Assistant reports; not independent device measurement timestamps"}
        });
        for (index, (name, result)) in BATTERY_NAMES
            .into_iter()
            .zip([soc, charge, discharge])
            .enumerate()
        {
            let (value, source) = match result {
                Ok(entity) => {
                    let reading = battery_value(&entity, index, now);
                    let status = match &reading {
                        Err(error) => diagnostic_status(error.as_ref()),
                        Ok(_) if now < entity.last_reported.unwrap_or(entity.last_updated) => {
                            "future_within_tolerance"
                        }
                        Ok(_)
                            if now - entity.last_reported.unwrap_or(entity.last_updated)
                                > chrono::Duration::seconds(300) =>
                        {
                            "last_known"
                        }
                        Ok(_) => "fresh",
                    };
                    (reading.ok(), battery_source(&entity, index, now, status))
                }
                Err(error) => (
                    None,
                    serde_json::json!({"entity_id":BATTERY_IDS[index],
                    "status":diagnostic_status(error.as_ref())}),
                ),
            };
            payload[name] = serde_json::json!(value);
            payload["freshness"]["sources"][name] = source;
        }
        payload["freshness"]["all_sources_recent"] =
            serde_json::json!(BATTERY_NAMES.into_iter().all(|name| matches!(
                payload["freshness"]["sources"][name]["status"].as_str(),
                Some("fresh" | "future_within_tolerance")
            )));
        payload
    }
}

const BATTERY_NAMES: [&str; 3] = ["soc_percent", "charge_power_w", "discharge_power_w"];
pub(crate) const BATTERY_IDS: [&str; 3] = [
    "sensor.solis_s6_eh1p_battery_soc",
    "sensor.solis_s6_eh1p_battery_charge_power",
    "sensor.solis_s6_eh1p_battery_discharge_power",
];

fn battery_value(
    entity: &EntityState,
    index: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<f64, ReadError> {
    let r = validate_with_age(entity, now, (index != 0).then_some(300))?;
    let value = match (index, r.unit.as_deref()) {
        (0, Some("%")) if (0.0..=100.0).contains(&r.value) => r.value,
        (0, _) => return Err(InvalidReading::InvalidSoc.into()),
        (_, Some("W")) => r.value,
        (_, Some("kW")) => r.value * 1000.0,
        _ => return Err(InvalidReading::InvalidUnit.into()),
    };
    if !value.is_finite() || value < 0.0 {
        return Err(InvalidReading::Invalid.into());
    }
    Ok(value)
}

fn battery_source(
    entity: &EntityState,
    index: usize,
    now: chrono::DateTime<chrono::Utc>,
    status: &str,
) -> serde_json::Value {
    let observed_at = entity.last_reported.unwrap_or(entity.last_updated);
    serde_json::json!({
        "entity_id":BATTERY_IDS[index],"status":status,
        "age_seconds":(now-observed_at).num_milliseconds() as f64 / 1000.0,
        "last_changed":entity.last_changed,"last_updated":entity.last_updated,"last_reported":entity.last_reported,
        "observed_at":observed_at,"observed_at_source":if entity.last_reported.is_some() {"last_reported"} else {"last_updated"},
    })
}

#[derive(serde::Serialize)]
pub struct Reading {
    pub value: f64,
    pub unit: Option<String>,
    pub last_updated: chrono::DateTime<chrono::Utc>,
    pub last_changed: chrono::DateTime<chrono::Utc>,
    pub observed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug)]
enum InvalidReading {
    Stale,
    Future,
    Invalid,
    InvalidSoc,
    InvalidUnit,
}
impl std::fmt::Display for InvalidReading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Stale => "stale",
                Self::Future => "future_timestamp",
                Self::Invalid => "invalid_state",
                Self::InvalidSoc => "invalid_soc",
                Self::InvalidUnit => "invalid_unit",
            }
        )
    }
}
impl std::error::Error for InvalidReading {}
pub fn reading_error_status(
    error: &(dyn std::error::Error + Send + Sync + 'static),
) -> &'static str {
    match error.downcast_ref::<InvalidReading>() {
        Some(InvalidReading::Stale) => "stale",
        Some(InvalidReading::Future) => "future_timestamp",
        Some(InvalidReading::Invalid) => "invalid_state",
        Some(InvalidReading::InvalidSoc) => "invalid_soc",
        Some(InvalidReading::InvalidUnit) => "invalid_unit",
        None => "upstream_error",
    }
}

// Only allowlisted labels reach logs. reqwest error text can contain request URLs.
pub fn diagnostic_status(error: &(dyn std::error::Error + Send + Sync + 'static)) -> &'static str {
    if let Some(error) = error.downcast_ref::<reqwest::Error>() {
        return match error.status().map(|status| status.as_u16()) {
            Some(401 | 403) => "ha_auth_rejected",
            Some(404) => "ha_entity_not_found",
            Some(_) => "ha_http_error",
            None if error.is_timeout() => "ha_timeout",
            None if error.is_connect() => "ha_connection_failed",
            None if error.is_decode() => "ha_invalid_response",
            None => "ha_request_failed",
        };
    }
    reading_error_status(error)
}

pub(crate) fn validate(
    entity: &EntityState,
    now: chrono::DateTime<chrono::Utc>,
    max_age: i64,
) -> Result<Reading, Box<dyn std::error::Error + Send + Sync>> {
    validate_with_age(entity, now, Some(max_age))
}

pub(crate) fn validate_with_age(
    entity: &EntityState,
    now: chrono::DateTime<chrono::Utc>,
    max_age: Option<i64>,
) -> Result<Reading, Box<dyn std::error::Error + Send + Sync>> {
    let observed_at = entity.last_reported.unwrap_or(entity.last_updated);
    let age = now.signed_duration_since(observed_at);
    if max_age.is_some_and(|limit| age > chrono::Duration::seconds(limit)) {
        return Err(InvalidReading::Stale.into());
    }
    if age < -chrono::Duration::seconds(30) {
        return Err(InvalidReading::Future.into());
    }
    if entity.last_updated > observed_at || entity.last_changed > entity.last_updated {
        return Err(InvalidReading::Invalid.into());
    }
    let value: f64 = entity.state.parse().map_err(|_| InvalidReading::Invalid)?;
    if !value.is_finite() {
        return Err(InvalidReading::Invalid.into());
    }
    Ok(Reading {
        value,
        unit: entity
            .attributes
            .get("unit_of_measurement")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        last_updated: entity.last_updated,
        last_changed: entity.last_changed,
        observed_at,
    })
}

impl HomeAssistantClient {
    pub async fn entity(
        &self,
        id: &str,
    ) -> Result<EntityState, Box<dyn std::error::Error + Send + Sync>> {
        if !(id.starts_with("sensor.") || id.starts_with("event."))
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
        {
            return Err("Invalid entity ID".into());
        }
        Ok(self
            .client
            .get(format!("{}/api/states/{}", self.base_url, id))
            .bearer_auth(&self.token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_use_allowlisted_labels() {
        for (error, label) in [
            (InvalidReading::Stale, "stale"),
            (InvalidReading::Future, "future_timestamp"),
            (InvalidReading::Invalid, "invalid_state"),
            (InvalidReading::InvalidUnit, "invalid_unit"),
            (InvalidReading::InvalidSoc, "invalid_soc"),
        ] {
            assert_eq!(diagnostic_status(&error), label);
        }
        let private_error: Box<dyn std::error::Error + Send + Sync> =
            "private-token-and-url".into();
        assert_eq!(diagnostic_status(private_error.as_ref()), "upstream_error");
    }

    #[test]
    fn rejects_stale_future_and_invalid_states() {
        let now = chrono::Utc::now();
        let mut e = EntityState {
            state: "12".into(),
            last_updated: now,
            last_changed: now,
            last_reported: None,
            attributes: serde_json::json!({}),
        };
        assert!(validate(&e, now, 300).is_ok());
        assert!(validate(&e, now + chrono::Duration::seconds(301), 300).is_err());
        assert!(validate(&e, now - chrono::Duration::seconds(31), 300).is_err());
        for bad in ["NaN", "inf", "unknown", "unavailable"] {
            e.state = bad.into();
            assert!(validate(&e, now, 300).is_err());
        }
    }
    #[test]
    fn unchanged_state_can_be_freshly_reported() {
        let now = chrono::Utc::now();
        let old = now - chrono::Duration::hours(1);
        let e = EntityState {
            state: "0".into(),
            last_updated: old,
            last_changed: old,
            last_reported: Some(now),
            attributes: serde_json::json!({}),
        };
        assert!(validate(&e, now, 300).is_ok());
    }
}
