use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

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

    pub async fn battery_snapshot(
        &self,
    ) -> Result<BatterySnapshot, Box<dyn std::error::Error + Send + Sync>> {
        let read = async |id: &'static str| {
            self.entity(id).await.map_err(|error| {
                eprintln!(
                    "Battery read failed: entity={id} reason={}",
                    diagnostic_status(error.as_ref())
                );
                error
            })
        };
        let (soc, charge, discharge) = tokio::try_join!(
            read("sensor.solis_s6_eh1p_battery_soc"),
            read("sensor.solis_s6_eh1p_battery_charge_power"),
            read("sensor.solis_s6_eh1p_battery_discharge_power"),
        )?;
        battery_snapshot_with_metadata([&soc, &charge, &discharge], chrono::Utc::now()).map_err(
            |error| {
                eprintln!(
                    "Battery snapshot rejected: reason={}",
                    diagnostic_status(error.as_ref())
                );
                error
            },
        )
    }
}

pub struct BatterySnapshot {
    pub values: [f64; 3],
    pub freshness: serde_json::Value,
}

fn battery_snapshot_with_metadata(
    entities: [&EntityState; 3],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<BatterySnapshot, Box<dyn std::error::Error + Send + Sync>> {
    let values = validate_battery_snapshot(entities, now)?;
    let mut sources = serde_json::Map::new();
    let ids = [
        "sensor.solis_s6_eh1p_battery_soc",
        "sensor.solis_s6_eh1p_battery_charge_power",
        "sensor.solis_s6_eh1p_battery_discharge_power",
    ];
    let names = ["soc_percent", "charge_power_w", "discharge_power_w"];
    let mut all_fresh = true;
    for (index, entity) in entities.into_iter().enumerate() {
        let observed_at = entity.last_reported.unwrap_or(entity.last_updated);
        let age = now.signed_duration_since(observed_at);
        let fresh = age <= chrono::Duration::seconds(300);
        all_fresh &= fresh;
        let status = if age < chrono::Duration::zero() {
            "future_within_tolerance"
        } else if fresh {
            "fresh"
        } else {
            "older_report_accepted_via_group"
        };
        sources.insert(names[index].into(),serde_json::json!({
            "entity_id":ids[index],"status":status,"age_seconds":age.num_milliseconds() as f64 / 1000.0,
            "last_changed":entity.last_changed,"last_updated":entity.last_updated,"last_reported":entity.last_reported,
            "observed_at":observed_at,"observed_at_source":if entity.last_reported.is_some() {"last_reported"} else {"last_updated"},
        }));
    }
    Ok(BatterySnapshot {
        values,
        freshness: serde_json::json!({
            "fetched_at":now,"status":"recent_group_activity","policy":"at_least_one_recent_inverter_report",
            "max_age_seconds":300,"future_tolerance_seconds":30,"all_sources_recent":all_fresh,"sources":sources,
            "timestamp_basis":"Home Assistant reports; not independent device measurement timestamps",
        }),
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

fn validate_with_age(
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

    pub async fn reading(
        &self,
        id: &str,
    ) -> Result<Reading, Box<dyn std::error::Error + Send + Sync>> {
        self.reading_with_max_age(id, 300).await
    }

    pub async fn reading_with_max_age(
        &self,
        id: &str,
        max_age: i64,
    ) -> Result<Reading, Box<dyn std::error::Error + Send + Sync>> {
        let entity = self.entity(id).await?;
        validate(&entity, chrono::Utc::now(), max_age)
    }
}

// These three known entities belong to the same inverter. A recent report from
// any member establishes group activity, not independent measurement freshness.
fn validate_battery_snapshot(
    entities: [&EntityState; 3],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<[f64; 3], Box<dyn std::error::Error + Send + Sync>> {
    let mut values = [0.0; 3];
    let mut active = false;
    for (index, entity) in entities.into_iter().enumerate() {
        let r = validate_with_age(entity, now, None)?;
        active |= now.signed_duration_since(r.observed_at) <= chrono::Duration::seconds(300);
        values[index] = match (index, r.unit.as_deref()) {
            (0, Some("%")) if (0.0..=100.0).contains(&r.value) => r.value,
            (0, _) => return Err(InvalidReading::InvalidSoc.into()),
            (_, Some("W")) => r.value,
            (_, Some("kW")) => r.value * 1000.0,
            _ => return Err(InvalidReading::InvalidUnit.into()),
        };
        if !values[index].is_finite() || values[index] < 0.0 {
            return Err(InvalidReading::Invalid.into());
        }
    }
    if !active {
        return Err(InvalidReading::Stale.into());
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn battery_metadata_preserves_old_reports_and_timestamp_fallback() {
        let now = chrono::Utc::now();
        let old = now - chrono::Duration::hours(1);
        let make = |state: &str, unit: &str, time, reported| EntityState {
            state: state.into(),
            attributes: serde_json::json!({"unit_of_measurement":unit}),
            last_changed: time,
            last_updated: time,
            last_reported: reported,
        };
        let soc = make("99", "%", old, None);
        let charge = make("0.978", "kW", now, Some(now));
        let discharge = make("0", "W", old, Some(old));
        let snapshot = battery_snapshot_with_metadata([&soc, &charge, &discharge], now).unwrap();
        assert_eq!(snapshot.values, [99.0, 978.0, 0.0]);
        let meta = snapshot.freshness;
        assert_eq!(meta["fetched_at"], serde_json::json!(now));
        assert_eq!(meta["all_sources_recent"], false);
        assert_eq!(meta["sources"]["charge_power_w"]["status"], "fresh");
        assert_eq!(
            meta["sources"]["discharge_power_w"]["status"],
            "older_report_accepted_via_group"
        );
        assert_eq!(
            meta["sources"]["discharge_power_w"]["observed_at"],
            serde_json::json!(old)
        );
        assert_eq!(meta["sources"]["discharge_power_w"]["age_seconds"], 3600.0);
        assert!(meta["sources"]["soc_percent"]["last_reported"].is_null());
        assert_eq!(
            meta["sources"]["soc_percent"]["observed_at_source"],
            "last_updated"
        );
        assert!(
            battery_snapshot_with_metadata(
                [&soc, &charge, &discharge],
                now + chrono::Duration::seconds(301)
            )
            .is_err()
        );
    }

    #[test]
    fn battery_group_allows_unchanged_values_but_requires_recent_activity() {
        let now = chrono::Utc::now();
        let old = now - chrono::Duration::hours(1);
        let make = |value: &str, unit: &str, time| EntityState {
            state: value.into(),
            attributes: serde_json::json!({"unit_of_measurement":unit}),
            last_changed: time,
            last_updated: time,
            last_reported: Some(time),
        };
        let mut soc = make("99", "%", old);
        let mut charge = make("0.978", "kW", now);
        let mut discharge = make("0", "W", old);
        assert_eq!(
            validate_battery_snapshot([&soc, &charge, &discharge], now).unwrap(),
            [99.0, 978.0, 0.0]
        );
        assert!(validate(&discharge, now, 300).is_err()); // Other telemetry stays strict.
        charge.last_reported = Some(old);
        charge.last_updated = old;
        charge.last_changed = old;
        let error = validate_battery_snapshot([&soc, &charge, &discharge], now).unwrap_err();
        assert_eq!(diagnostic_status(error.as_ref()), "stale");
        charge.last_reported = Some(now);
        for bad in ["unavailable", "unknown", "NaN", "-1"] {
            discharge.state = bad.into();
            assert!(validate_battery_snapshot([&soc, &charge, &discharge], now).is_err());
        }
        discharge.state = "0".into();
        discharge.last_reported = Some(now + chrono::Duration::seconds(31));
        assert!(validate_battery_snapshot([&soc, &charge, &discharge], now).is_err());
        discharge.last_reported = Some(old);
        discharge.attributes["unit_of_measurement"] = "kWh".into();
        assert!(validate_battery_snapshot([&soc, &charge, &discharge], now).is_err());
        discharge.attributes["unit_of_measurement"] = "W".into();
        soc.state = "101".into();
        assert!(validate_battery_snapshot([&soc, &charge, &discharge], now).is_err());
    }

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
