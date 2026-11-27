use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::{sync::watch, time::Instant};

use crate::{
    api::{AppState, control_solar},
    home_assistant::{EntityState, diagnostic_status, validate_with_age},
};

const POLL: Duration = Duration::from_secs(30);
const DWELL: Duration = Duration::from_secs(120);
const ZERO_GENERATION_TARGET: u8 = 90;
const RETRY: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Off,
    DryRun,
    Active,
}

impl Mode {
    pub(crate) fn from_env() -> Result<Self, &'static str> {
        match std::env::var("HEMS_EXPORT_CALIBRATION_MODE").as_deref() {
            Err(std::env::VarError::NotPresent) | Ok("off") => Ok(Self::Off),
            Ok("dry-run") => Ok(Self::DryRun),
            Ok("active") => Ok(Self::Active),
            _ => Err("HEMS_EXPORT_CALIBRATION_MODE must be off, dry-run, or active"),
        }
    }
}

struct Candidate {
    target: u8,
    started: Instant,
    reports: [DateTime<Utc>; 3],
}

#[derive(Default)]
struct Controller {
    candidate: Option<Candidate>,
    last_attempt: Option<Instant>,
    previewed: Option<u8>,
}

fn target(watts: f64) -> Option<u8> {
    if watts > 100.0 {
        Some(30)
    } else if watts == 0.0 {
        Some(ZERO_GENERATION_TARGET)
    } else {
        None
    }
}

fn calibration_value(entity: &EntityState, now: DateTime<Utc>) -> Result<f64, &'static str> {
    let reading =
        validate_with_age(entity, now, None).map_err(|e| diagnostic_status(e.as_ref()))?;
    let [Some(min), Some(max), Some(step)] =
        ["min", "max", "step"].map(|name| entity.attributes[name].as_f64())
    else {
        return Err("missing_calibration_range");
    };
    if step <= 0.0
        || ![reading.value, 30.0, 90.0].iter().all(|value| {
            let steps = (value - min) / step;
            (min..=max).contains(value) && (steps - steps.round()).abs() <= 1e-7
        })
    {
        return Err("unsupported_calibration_range_or_step");
    }
    Ok(reading.value)
}

impl Controller {
    fn reset(&mut self) {
        self.candidate = None;
        self.previewed = None;
    }

    fn ready(&mut self, target: u8, reports: [DateTime<Utc>; 3], now: Instant) -> bool {
        if let Some(candidate) = &self.candidate
            && candidate.target == target
            && reports
                .iter()
                .zip(candidate.reports)
                .all(|(report, first)| *report >= first)
        {
            return now.duration_since(candidate.started) >= DWELL
                && (target == ZERO_GENERATION_TARGET
                    || reports
                        .iter()
                        .zip(candidate.reports)
                        .all(|(report, first)| *report > first));
        }
        self.previewed = None;
        self.candidate = Some(Candidate {
            target,
            started: now,
            reports,
        });
        false
    }

    async fn poll(&mut self, mode: Mode, state: &AppState) -> Result<&'static str, &'static str> {
        if mode == Mode::Off {
            return Ok("off");
        }
        if mode == Mode::Active && !override_off(state).await? {
            self.reset();
            return Ok("manual_override");
        }
        let (watts, reports, fit_asleep) = control_solar(state).await?;
        let fit_note = if fit_asleep {
            " (FIT unavailable, treated as zero)"
        } else {
            ""
        };
        let Some(target) = target(watts) else {
            self.reset();
            return Ok("hold");
        };
        let now = Instant::now();
        if !self.ready(target, reports, now) {
            return Ok("waiting_for_sustained_solar");
        }
        let entity = state
            .ha
            .calibration_state()
            .await
            .map_err(|e| diagnostic_status(e.as_ref()))?;
        let previous = calibration_value(&entity, Utc::now())?;
        if previous == f64::from(target) {
            self.previewed = None;
            return Ok("already_set");
        }
        if mode == Mode::DryRun {
            if self.previewed != Some(target) {
                println!(
                    "{} Export calibration dry run: {previous} -> {target}, solar {watts} W{fit_note}",
                    Utc::now()
                );
                self.previewed = Some(target);
            }
            return Ok("dry_run");
        }
        if self
            .last_attempt
            .is_some_and(|at| now.duration_since(at) < RETRY)
        {
            return Ok("write_cooldown");
        }
        // Recheck the override immediately before submitting the action.
        if !override_off(state).await? {
            self.reset();
            return Ok("manual_override");
        }
        let checked_at = Utc::now();
        if reports.iter().any(|report| {
            *report > checked_at
                || (target != ZERO_GENERATION_TARGET
                    && checked_at - *report > chrono::Duration::seconds(300))
        }) {
            return Err("solar_expired_before_write");
        }
        self.last_attempt = Some(Instant::now());
        self.reset();
        println!(
            "{} Export calibration requesting {previous} -> {target}, solar {watts} W{fit_note}",
            Utc::now()
        );
        state
            .ha
            .set_export_calibration(target)
            .await
            .map_err(|e| diagnostic_status(e.as_ref()))?;
        let message = format!(
            "Home Assistant accepted the request to change Solis export calibration from {previous} to {target} at {}. Solar total used for control was {watts} W{fit_note}.",
            Utc::now().to_rfc3339()
        );
        println!("{message}");
        if let Err(error) = state.ha.notify_export_calibration(&message).await {
            eprintln!(
                "Export calibration notification failed: {}",
                diagnostic_status(error.as_ref())
            );
        }
        Ok("write_accepted")
    }
}

async fn override_off(state: &AppState) -> Result<bool, &'static str> {
    state
        .ha
        .calibration_override_off()
        .await
        .map_err(|_| "manual_override_unavailable")
}

pub(crate) fn start(
    mode: Mode,
    state: AppState,
    mut stop: watch::Receiver<bool>,
) -> Option<tokio::task::JoinHandle<()>> {
    if mode == Mode::Off {
        return None;
    }
    Some(tokio::spawn(async move {
        println!("Export calibration mode: {mode:?}");
        let mut timer = tokio::time::interval(POLL);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut controller = Controller::default();
        let mut last_status = None;
        loop {
            tokio::select! {
                biased;
                _ = stop.changed() => break,
                _ = timer.tick() => {
                    let status = controller.poll(mode, &state).await.unwrap_or_else(|reason| {
                        controller.reset();
                        reason
                    });
                    if last_status != Some(status) {
                        println!("{} Export calibration: {status}", Utc::now());
                        last_status = Some(status);
                    }
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::Path,
        routing::{get, post},
    };
    use serde_json::{Value, json};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering::Relaxed},
    };

    #[test]
    fn dwell_requires_new_reports_for_generation_but_accepts_unchanged_zero() {
        for (watts, expected) in [
            (0.0, Some(90)),
            (0.1, None),
            (1.0, None),
            (100.0, None),
            (100.1, Some(30)),
        ] {
            assert_eq!(target(watts), expected);
        }
        let mut controller = Controller::default();
        let start = Instant::now();
        let first = Utc::now();
        let next = first + chrono::Duration::seconds(60);
        assert!(!controller.ready(30, [first; 3], start));
        assert!(!controller.ready(30, [next; 3], start + DWELL - POLL));
        assert!(!controller.ready(30, [next, next, first], start + DWELL));
        assert!(controller.ready(30, [next; 3], start + DWELL));
        assert!(!controller.ready(90, [next; 3], start + DWELL));
        assert!(controller.ready(90, [next; 3], start + DWELL * 2));
        controller.reset();
        assert!(!controller.ready(30, [next; 3], start + DWELL * 2));
    }

    fn entity(value: &str, at: DateTime<Utc>, attributes: Value) -> Value {
        json!({"state": value, "last_changed": at, "last_updated": at,
               "last_reported": at, "attributes": attributes})
    }

    #[test]
    fn checks_both_presets_against_range_and_step() {
        let now = Utc::now();
        for (attributes, valid) in [
            (json!({"min": 0, "max": 100, "step": 1}), true),
            (json!({"min": 0, "max": 80, "step": 1}), false),
            (json!({"min": 0, "max": 100, "step": 4}), false),
            (json!({"min": 0, "max": 100, "step": 0}), false),
            (json!({"min": 0, "max": 100}), false),
        ] {
            let state = serde_json::from_value(entity("50", now, attributes)).unwrap();
            assert_eq!(calibration_value(&state, now).is_ok(), valid);
        }
    }

    #[tokio::test]
    async fn mocked_ha_blocks_unsafe_actions_and_audits_an_idempotent_write() {
        use crate::home_assistant::{EXPORT_CALIBRATION, EXPORT_OVERRIDE};
        let calls = Arc::new(Mutex::new(Vec::new()));
        let value = Arc::new(AtomicI64::new(90));
        let paused = Arc::new(AtomicBool::new(true));
        let age = Arc::new(AtomicI64::new(301));
        let (read_value, read_paused, read_age) = (value.clone(), paused.clone(), age.clone());
        let night = Arc::new(AtomicBool::new(false));
        let fit_down = Arc::new(AtomicBool::new(false));
        let read_fit_down = fit_down.clone();
        let read_night = night.clone();
        let (post_value, posts) = (value.clone(), calls.clone());
        let ha = Router::new()
            .route(
                "/api/states/{id}",
                get(move |Path(id): Path<String>| {
                    let (value, paused, age) =
                        (read_value.clone(), read_paused.clone(), read_age.clone());
                    let night = read_night.clone();
                    let fit_down = read_fit_down.clone();
                    async move {
                        let now = Utc::now();
                        let solar = |day| if night.load(Relaxed) { "0" } else { day };
                        Json(match id.as_str() {
                            EXPORT_CALIBRATION => entity(
                                &value.load(Relaxed).to_string(),
                                now,
                                json!({"min":0,"max":100,"step":1}),
                            ),
                            EXPORT_OVERRIDE => entity(
                                if paused.load(Relaxed) { "on" } else { "off" },
                                now,
                                json!({}),
                            ),
                            "sensor.solis_s6_gr1p_active_power" => entity(
                                if fit_down.load(Relaxed) {
                                    "unavailable"
                                } else {
                                    solar("0.020")
                                },
                                now - chrono::Duration::seconds(age.load(Relaxed)),
                                json!({"unit_of_measurement":"kW"}),
                            ),
                            "sensor.solis_s6_eh1p_pv_power_1" => entity(
                                solar("10"),
                                if night.load(Relaxed) {
                                    now - chrono::Duration::minutes(50)
                                } else {
                                    now
                                },
                                json!({"unit_of_measurement":"W"}),
                            ),
                            "sensor.solis_s6_eh1p_pv_power_2" => {
                                entity(solar("80"), now, json!({"unit_of_measurement":"W"}))
                            }
                            _ => panic!("Unexpected sensor: {id}"),
                        })
                    }
                }),
            )
            .route(
                "/api/services/{domain}/{action}",
                post(
                    move |Path(path): Path<(String, String)>, Json(body): Json<Value>| {
                        let (value, posts) = (post_value.clone(), posts.clone());
                        async move {
                            if path.0 == "number" {
                                value.store(body["value"].as_i64().unwrap(), Relaxed);
                            }
                            posts.lock().unwrap().push((path, body));
                            Json(json!([]))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, ha).await.unwrap() });
        let state = AppState {
            ha: crate::home_assistant::HomeAssistantClient::new(url, "fixture".into()).unwrap(),
            collection: crate::api::CollectionConfig::from_env().unwrap(),
        };

        let mut controller = Controller::default();
        assert_eq!(
            controller.poll(Mode::Active, &state).await,
            Ok("manual_override")
        );
        paused.store(false, Relaxed);
        assert_eq!(controller.poll(Mode::Active, &state).await, Err("stale"));
        age.store(121, Relaxed);
        assert_eq!(
            controller.poll(Mode::Active, &state).await,
            Err("invalid_or_misaligned_solar_total")
        );
        age.store(0, Relaxed);
        assert_eq!(control_solar(&state).await.unwrap().0, 110.0);

        // Age the dwell clock without waiting two real minutes.
        let ready = |target| Candidate {
            target,
            started: Instant::now() - DWELL,
            reports: [Utc::now() - chrono::Duration::seconds(120); 3],
        };
        controller.candidate = Some(ready(30));
        assert_eq!(controller.poll(Mode::DryRun, &state).await, Ok("dry_run"));
        assert!(calls.lock().unwrap().is_empty());

        assert_eq!(
            controller.poll(Mode::Active, &state).await,
            Ok("write_accepted")
        );
        controller.candidate = Some(ready(30));
        assert_eq!(
            controller.poll(Mode::Active, &state).await,
            Ok("already_set")
        );
        fit_down.store(true, Relaxed);
        assert_eq!(
            control_solar(&state).await,
            Err("fit_unavailable_with_nonzero_hybrid_pv")
        );
        night.store(true, Relaxed);
        age.store(3000, Relaxed);
        let (watts, reports, fit_asleep) = control_solar(&state).await.unwrap();
        assert_eq!((watts, fit_asleep), (0.0, true));
        controller.candidate = Some(Candidate {
            reports,
            ..ready(90)
        });
        assert_eq!(
            controller.poll(Mode::Active, &state).await,
            Ok("write_cooldown")
        );
        controller.last_attempt = Some(Instant::now() - RETRY);
        assert_eq!(
            controller.poll(Mode::Active, &state).await,
            Ok("write_accepted")
        );
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[2].1["value"], 90);
        assert!(
            calls[3].1["message"]
                .as_str()
                .unwrap()
                .contains("FIT unavailable, treated as zero")
        );
        assert_eq!(
            calls[0],
            (
                ("number".into(), "set_value".into()),
                json!({"entity_id": EXPORT_CALIBRATION, "value": 30})
            )
        );
        assert_eq!(
            calls[1].0,
            ("persistent_notification".into(), "create".into())
        );
        assert!(calls[1].1["message"].as_str().unwrap().contains("110 W"));
        server.abort();
    }
}
