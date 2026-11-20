use axum::{Json, extract::State};
use chrono::{DateTime, Datelike, Utc};
use serde_json::{Value, json};

use super::AppState;
use crate::home_assistant::{EntityState, diagnostic_status, validate};

const MAX_AGE_SECONDS: i64 = 300;

#[derive(Clone, Copy)]
enum Period {
    Day,
    Month,
    Year,
    Cumulative,
}

impl Period {
    fn contains(self, report: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        let report = report.with_timezone(&chrono_tz::Europe::London);
        let now = now.with_timezone(&chrono_tz::Europe::London);
        match self {
            Self::Day => report.date_naive() == now.date_naive(),
            Self::Month => (report.year(), report.month()) == (now.year(), now.month()),
            Self::Year => report.year() == now.year(),
            Self::Cumulative => true,
        }
    }
}

fn export_reading(entity: &EntityState, period: Period, now: DateTime<Utc>) -> Value {
    let observed_at = entity.last_reported.unwrap_or(entity.last_updated);
    let (status, value) = match validate(entity, now, MAX_AGE_SECONDS) {
        Err(error) => (diagnostic_status(error.as_ref()), None),
        Ok(_) if !period.contains(observed_at, now) => ("stale", None),
        Ok(reading) if reading.value < 0.0 => ("invalid_state", None),
        Ok(reading) => match reading.unit.as_deref() {
            Some("kWh") => ("valid", Some(reading.value)),
            Some("Wh") => ("valid", Some(reading.value / 1000.0)),
            _ => ("invalid_unit", None),
        },
    };
    json!({
        "status": status,
        "reading": {
            "value": value,
            "unit": "kWh",
            "observed_at": observed_at,
            "last_changed": entity.last_changed,
            "last_updated": entity.last_updated,
            "last_reported": entity.last_reported,
        }
    })
}

pub(super) async fn get(State(state): State<AppState>) -> Json<Value> {
    const DAILY: &str = "sensor.octopus_grid_export_daily";
    const MONTHLY: &str = "sensor.octopus_grid_export_monthly";
    const YEARLY: &str = "sensor.octopus_grid_export_yearly";
    const CUMULATIVE: &str = "sensor.octopus_grid_export_energy";
    let (day, month, year, cumulative) = tokio::join!(
        state.ha.entity(DAILY),
        state.ha.entity(MONTHLY),
        state.ha.entity(YEARLY),
        state.ha.entity(CUMULATIVE),
    );
    let now = Utc::now();
    let entry =
        |id, period, result: Result<EntityState, Box<dyn std::error::Error + Send + Sync>>| {
            let mut value = match result {
                Ok(entity) => export_reading(&entity, period, now),
                Err(error) => json!({"status": diagnostic_status(error.as_ref())}),
            };
            value["entity_id"] = json!(id);
            value
        };
    Json(json!({
        "daily": entry(DAILY, Period::Day, day),
        "monthly": entry(MONTHLY, Period::Month, month),
        "yearly": entry(YEARLY, Period::Year, year),
        "diagnostics": {"cumulative": entry(CUMULATIVE, Period::Cumulative, cumulative)},
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(state: &str, unit: &str, observed_at: DateTime<Utc>) -> EntityState {
        serde_json::from_value(json!({
            "state": state,
            "attributes": {"unit_of_measurement": unit},
            "last_updated": observed_at,
            "last_changed": observed_at,
        }))
        .unwrap()
    }

    #[test]
    fn validates_energy_and_preserves_report_timestamp() {
        let now = "2026-07-14T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let valid = export_reading(&entity("0", "Wh", now), Period::Day, now);
        assert_eq!(valid["reading"]["value"], 0.0);
        assert_eq!(valid["reading"]["observed_at"], json!(now));

        for (state, unit, offset, status) in [
            ("12", "kWh", -301, "stale"),
            ("12", "kWh", 31, "future_timestamp"),
            ("unavailable", "kWh", 0, "invalid_state"),
            ("-1", "kWh", 0, "invalid_state"),
            ("NaN", "kWh", 0, "invalid_state"),
            ("12", "kW", 0, "invalid_unit"),
        ] {
            let observed_at = now + chrono::Duration::seconds(offset);
            let result = export_reading(&entity(state, unit, observed_at), Period::Day, now);
            assert_eq!(result["status"], status);
            assert!(result["reading"]["value"].is_null());
            assert_eq!(result["reading"]["observed_at"], json!(observed_at));
        }
    }

    #[test]
    fn london_midnight_expires_the_period_that_ended() {
        let now = "2026-06-30T23:00:01Z".parse::<DateTime<Utc>>().unwrap();
        let report = entity("12", "kWh", now - chrono::Duration::seconds(2));
        for period in [Period::Day, Period::Month] {
            assert_eq!(export_reading(&report, period, now)["status"], "stale");
        }
        assert_eq!(
            export_reading(&report, Period::Year, now)["status"],
            "valid"
        );

        let now = "2027-01-01T00:00:01Z".parse::<DateTime<Utc>>().unwrap();
        let report = entity("12", "kWh", now - chrono::Duration::seconds(2));
        assert_eq!(
            export_reading(&report, Period::Year, now)["status"],
            "stale"
        );
        assert_eq!(
            export_reading(&report, Period::Cumulative, now)["status"],
            "valid"
        );
    }
}
