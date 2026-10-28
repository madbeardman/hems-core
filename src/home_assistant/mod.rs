use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub struct EntityState {
    state: String,
    pub last_updated: chrono::DateTime<chrono::Utc>,
    last_changed: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    last_reported: Option<chrono::DateTime<chrono::Utc>>,
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

    pub async fn get_numeric_state(
        &self,
        entity_id: &str,
    ) -> Result<f64, Box<dyn std::error::Error + Send + Sync>> {
        let mut r = self.reading(entity_id).await?;
        if entity_id.ends_with("_soc") {
            if r.unit.as_deref() != Some("%") || !(0.0..=100.0).contains(&r.value) {
                return Err("Invalid SOC".into());
            }
        } else {
            match r.unit.as_deref() {
                Some("W") => (),
                Some("kW") => r.value *= 1000.0,
                _ => return Err("Invalid battery power unit".into()),
            }
        }
        if !r.value.is_finite() {
            return Err("Invalid normalized battery power".into());
        }
        let value = r.value;

        Ok(value)
    }
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
        None => "upstream_error",
    }
}

fn validate(
    entity: &EntityState,
    now: chrono::DateTime<chrono::Utc>,
    max_age: i64,
) -> Result<Reading, Box<dyn std::error::Error + Send + Sync>> {
    let observed_at = entity.last_reported.unwrap_or(entity.last_updated);
    let age = now.signed_duration_since(observed_at);
    if age > chrono::Duration::seconds(max_age) {
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
        let entity = self.entity(id).await?;
        validate(&entity, chrono::Utc::now(), 300)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
