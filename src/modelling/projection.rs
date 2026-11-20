//! Self-consumption scenario from stored power samples and explicit conversion assumptions.
use super::{Error, Sample, Settings as ModelSettings, daylight, n, percentile};
use chrono::{DateTime, NaiveDate, Timelike, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    ops::Range,
};

const SLOT: i64 = 1800;
const DAY: i64 = 86400;
const MIN_DAYS: usize = 3;
const SOLAR_DAYS: i64 = 7;

#[derive(Clone, Serialize)]
pub(super) struct Settings {
    #[serde(skip)]
    pub(super) site: Option<(f64, f64)>,
    hybrid_dc_to_ac: f64,
    pv_dc_to_stored: f64,
    fit_ac_to_stored: f64,
    stored_to_ac: f64,
    max_charge_stored_kw: f64,
    max_discharge_ac_kw: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            site: None,
            hybrid_dc_to_ac: 0.96,
            pv_dc_to_stored: 0.95,
            fit_ac_to_stored: 0.92,
            stored_to_ac: 0.92,
            max_charge_stored_kw: 3.,
            max_discharge_ac_kw: 3.,
        }
    }
}

impl Settings {
    pub(super) fn from_env() -> Result<Self, Error> {
        let d = Self::default();
        let lat = std::env::var("HEMS_PROJECTION_LATITUDE")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let lon = std::env::var("HEMS_PROJECTION_LONGITUDE")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let site = match (lat, lon) {
            (None, None) => None,
            (Some(lat), Some(lon)) => {
                match (lat.trim().parse::<f64>(), lon.trim().parse::<f64>()) {
                    (Ok(lat), Ok(lon))
                        if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) =>
                    {
                        Some((lat, lon))
                    }
                    _ => return Err("Invalid projection latitude or longitude".into()),
                }
            }
            _ => return Err("Set both projection latitude and longitude".into()),
        };
        Ok(Self {
            site,
            hybrid_dc_to_ac: n(
                "HEMS_PROJECTION_HYBRID_DC_TO_AC",
                d.hybrid_dc_to_ac,
                0.01,
                1.,
            )?,
            pv_dc_to_stored: n(
                "HEMS_PROJECTION_PV_DC_TO_STORED",
                d.pv_dc_to_stored,
                0.01,
                1.,
            )?,
            fit_ac_to_stored: n(
                "HEMS_PROJECTION_FIT_AC_TO_STORED",
                d.fit_ac_to_stored,
                0.01,
                1.,
            )?,
            stored_to_ac: n("HEMS_PROJECTION_STORED_TO_AC", d.stored_to_ac, 0.01, 1.)?,
            max_charge_stored_kw: n(
                "HEMS_PROJECTION_MAX_CHARGE_STORED_KW",
                d.max_charge_stored_kw,
                0.,
                100.,
            )?,
            max_discharge_ac_kw: n(
                "HEMS_PROJECTION_MAX_DISCHARGE_AC_KW",
                d.max_discharge_ac_kw,
                0.,
                100.,
            )?,
        })
    }
}

#[derive(Clone, Copy, Default)]
struct Coverage {
    seconds: i64,
    watt_seconds: f64,
    available_at: i64,
}

impl Coverage {
    fn mean(self) -> Option<f64> {
        (self.seconds >= SLOT * 9 / 10).then(|| self.watt_seconds / self.seconds as f64)
    }
}

fn bins(series: impl IntoIterator<Item = (i64, i64, f64)>) -> BTreeMap<i64, Coverage> {
    let mut bins = BTreeMap::<i64, Coverage>::new();
    let mut previous = None;
    for (at, fetched, value) in series {
        if let Some((since, previous_fetch, watts)) = previous
            && (1..=120).contains(&(at - since))
        {
            // Hold the earlier sample for at most two minutes. This also keeps
            // direction changes from cancelling import/export across a gap.
            let mut start: i64 = since;
            while start < at {
                let slot = start.div_euclid(SLOT) * SLOT;
                let end = at.min(slot + SLOT);
                let entry = bins.entry(slot).or_default();
                entry.seconds += end - start;
                entry.available_at = entry.available_at.max(fetched).max(previous_fetch);
                entry.watt_seconds += watts * (end - start) as f64;
                start = end;
            }
        }
        previous = Some((at, fetched, value));
    }
    bins
}

struct History<'a> {
    demand: BTreeMap<i64, Coverage>,
    fit: BTreeMap<i64, Coverage>,
    hybrid: BTreeMap<i64, Coverage>,
    import: BTreeMap<i64, Coverage>,
    export: BTreeMap<i64, Coverage>,
    soc: Vec<&'a Sample>,
    demand_sources: [usize; 2],
}

impl<'a> History<'a> {
    fn new(samples: &'a [Sample]) -> Self {
        let series = |metric| samples.iter().filter(move |s| s.metric == metric);
        let values = |metric| series(metric).map(|s| (s.at, s.fetched, s.value));
        let mut demand = BTreeMap::new();
        let mut counts = [0, 0];
        for (source, name) in ["household_ac_balance", "household"]
            .into_iter()
            .enumerate()
        {
            for s in series(name) {
                if (source == 0 || s.value > 0.)
                    && let std::collections::btree_map::Entry::Vacant(entry) = demand.entry(s.at)
                {
                    entry.insert((s.fetched, s.value));
                    counts[source] += 1;
                }
            }
        }
        let pv2: HashMap<_, _> = values("hybrid_pv_2")
            .map(|(at, fetched, w)| (at, (fetched, w)))
            .collect();
        Self {
            demand: bins(
                demand
                    .into_iter()
                    .map(|(at, (fetched, w))| (at, fetched, w)),
            ),
            fit: bins(values("fit_pv")),
            hybrid: bins(values("hybrid_pv_1").filter_map(|(at, fetched, w)| {
                let &(other_fetch, other_w) = pv2.get(&at)?;
                Some((at, fetched.max(other_fetch), w + other_w))
            })),
            import: bins(values("grid_net").map(|(at, fetched, w)| (at, fetched, w.max(0.)))),
            export: bins(values("grid_net").map(|(at, fetched, w)| (at, fetched, (-w).max(0.)))),
            soc: series("soc_percent").collect(),
            demand_sources: counts,
        }
    }

    fn soc_at(&self, time: i64) -> Option<f64> {
        let end = self.soc.partition_point(|s| s.at <= time);
        self.soc[..end]
            .iter()
            .rev()
            .take_while(|s| s.at >= time - 300)
            .find(|s| s.fetched <= time && (0..=300).contains(&(time - s.observed)))
            .map(|s| s.value)
    }
}

fn instant(at: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(at, 0).unwrap()
}

fn london_slot(at: i64) -> (NaiveDate, u32) {
    let t = instant(at).with_timezone(&chrono_tz::Europe::London);
    (t.date_naive(), t.hour() * 2 + t.minute() / 30)
}

fn estimate(
    bins: &BTreeMap<i64, Coverage>,
    origin: i64,
    target: i64,
    days: i64,
    occupancy: Option<&ModelSettings>,
    solar_windows: Option<&[Range<i64>]>,
) -> (Option<f64>, usize) {
    let (_, slot) = london_slot(target);
    let target_occupancy = occupancy.map(|cfg| cfg.occupancy(instant(target)));
    let mut dates = BTreeMap::<NaiveDate, (f64, usize)>::new();
    for (&at, &bin) in bins.range(origin - days * DAY..=origin - SLOT) {
        let (date, candidate_slot) = london_slot(at);
        if bin.available_at <= origin
            && candidate_slot == slot
            && solar_windows.is_none_or(|w| daylight::overlaps(w, at, at + SLOT))
            && occupancy.is_none_or(|cfg| Some(cfg.occupancy(instant(at))) == target_occupancy)
            && let Some(mean) = bin.mean()
        {
            let entry = dates.entry(date).or_default();
            entry.0 += mean;
            entry.1 += 1;
        }
    }
    let count = dates.len();
    let median = (count >= MIN_DAYS).then(|| {
        let mut watts: Vec<_> = dates.values().map(|(sum, n)| sum / *n as f64).collect();
        percentile(&mut watts, 0.5).unwrap()
    });
    (median, count)
}

#[derive(Debug, Serialize)]
struct Flow {
    stored_kwh: f64,
    charge_stored_kwh: f64,
    discharge_stored_kwh: f64,
    grid_import_kwh: f64,
    grid_export_kwh: f64,
}

fn dispatch(
    cfg: &ModelSettings,
    stored: f64,
    demand: f64,
    fit_ac: f64,
    hybrid_dc: f64,
    hours: f64,
) -> Flow {
    let p = &cfg.projection;
    let after_fit = (demand - fit_ac).max(0.);
    let dc_to_load = hybrid_dc.min(after_fit / p.hybrid_dc_to_ac);
    let unmet = (after_fit - dc_to_load * p.hybrid_dc_to_ac).max(0.);
    let excess_dc = hybrid_dc - dc_to_load;
    let excess_ac = (fit_ac - demand).max(0.);
    let room = (cfg.usable - stored)
        .max(0.)
        .min(p.max_charge_stored_kw * hours);
    let charge_dc = (excess_dc * p.pv_dc_to_stored).min(room);
    let charge_ac = (excess_ac * p.fit_ac_to_stored).min(room - charge_dc);
    let charge = charge_dc + charge_ac;
    let reserve = cfg.usable * cfg.reserve / 100.;
    let discharge_ac = unmet
        .min(p.max_discharge_ac_kw * hours)
        .min((stored - reserve).max(0.) * p.stored_to_ac);
    let discharge = discharge_ac / p.stored_to_ac;
    Flow {
        stored_kwh: stored + charge - discharge,
        charge_stored_kwh: charge,
        discharge_stored_kwh: discharge,
        grid_import_kwh: unmet - discharge_ac,
        grid_export_kwh: (excess_dc - charge_dc / p.pv_dc_to_stored).max(0.) * p.hybrid_dc_to_ac
            + (excess_ac - charge_ac / p.fit_ac_to_stored).max(0.),
    }
}

#[derive(Serialize)]
struct Interval {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    local_start: String,
    demand_days: usize,
    demand_occupancy_pooled: bool,
    demand_baseload_fallback: bool,
    solar_nighttime_zero: bool,
    fit_days: usize,
    hybrid_days: usize,
    demand_ac_kwh: Option<f64>,
    fit_ac_kwh: Option<f64>,
    hybrid_dc_kwh: Option<f64>,
    soc_end_percent: Option<f64>,
    flow: Option<Flow>,
}

fn horizon(
    h: &History,
    cfg: &ModelSettings,
    origin: i64,
    soc: Option<f64>,
    use_baseload: bool,
) -> Vec<Interval> {
    let mut stored = soc.map(|s| cfg.usable * s / 100.);
    let solar_windows =
        daylight::relevant_windows(origin - SOLAR_DAYS * DAY..origin + DAY, cfg.projection.site);
    let mut start = origin;
    let mut intervals = Vec::with_capacity(49);
    while start < origin + DAY {
        let end = ((start.div_euclid(SLOT) + 1) * SLOT).min(origin + DAY);
        let hours = (end - start) as f64 / 3600.;
        let (matched, matched_days) = estimate(&h.demand, origin, start, 28, Some(cfg), None);
        let (demand, demand_days, demand_occupancy_pooled) = if matched.is_some() {
            (matched, matched_days, false)
        } else {
            let (pooled, pooled_days) = estimate(&h.demand, origin, start, 28, None, None);
            (pooled, pooled_days, pooled.is_some())
        };
        let solar_nighttime_zero = !daylight::overlaps(&solar_windows, start, end);
        let (fit, fit_days, hybrid, hybrid_days) = if solar_nighttime_zero {
            (Some(0.), 0, Some(0.), 0)
        } else {
            let (fit, fit_days) = estimate(
                &h.fit,
                origin,
                start,
                SOLAR_DAYS,
                None,
                Some(&solar_windows),
            );
            let (hybrid, hybrid_days) = estimate(
                &h.hybrid,
                origin,
                start,
                SOLAR_DAYS,
                None,
                Some(&solar_windows),
            );
            (fit, fit_days, hybrid, hybrid_days)
        };
        let demand_baseload_fallback = demand.is_none() && use_baseload && cfg.base > 0.;
        let kwh = |watts| watts * hours / 1000.;
        let demand = demand
            .or(demand_baseload_fallback.then_some(cfg.base))
            .map(|w| kwh(w.max(cfg.base)));
        let fit = fit.map(kwh);
        let hybrid = hybrid.map(kwh);
        let flow = stored.zip(demand).map(|(stored, demand)| {
            dispatch(
                cfg,
                stored,
                demand,
                fit.unwrap_or(0.),
                hybrid.unwrap_or(0.),
                hours,
            )
        });
        stored = flow.as_ref().map(|f| f.stored_kwh);
        intervals.push(Interval {
            start: instant(start),
            end: instant(end),
            local_start: instant(start)
                .with_timezone(&chrono_tz::Europe::London)
                .to_rfc3339(),
            demand_days,
            demand_occupancy_pooled,
            demand_baseload_fallback,
            solar_nighttime_zero,
            fit_days,
            hybrid_days,
            demand_ac_kwh: demand,
            fit_ac_kwh: fit,
            hybrid_dc_kwh: hybrid,
            soc_end_percent: stored.map(|kwh| kwh / cfg.usable * 100.),
            flow,
        });
        start = end;
    }
    intervals
}

#[derive(Default, Serialize)]
struct Score {
    paired_intervals: usize,
    mean_absolute_error: Option<f64>,
}

fn score(errors: impl Iterator<Item = Option<f64>>) -> Score {
    let (sum, n) = errors
        .flatten()
        .fold((0., 0), |(sum, n), e| (sum + e.abs(), n + 1));
    Score {
        paired_intervals: n,
        mean_absolute_error: (n > 0).then(|| sum / n as f64),
    }
}

fn backtest(h: &History, cfg: &ModelSettings, now: i64) -> Value {
    let latest = now.div_euclid(SLOT) * SLOT;
    let mut runs = Vec::new();
    for day in 1..=3 {
        let origin = latest - day * DAY;
        let rows = horizon(h, cfg, origin, h.soc_at(origin), false);
        let grid = |bins: &BTreeMap<i64, Coverage>, import: bool| {
            score(rows.iter().map(|r| {
                let actual = bins.get(&r.start.timestamp())?.mean()? / 2000.;
                let f = r.flow.as_ref()?;
                Some(
                    if import {
                        f.grid_import_kwh
                    } else {
                        f.grid_export_kwh
                    } - actual,
                )
            }))
        };
        let soc = score(
            rows.iter()
                .map(|r| Some(r.soc_end_percent? - h.soc_at(r.end.timestamp())?)),
        );
        let demand =
            score(rows.iter().map(|r| {
                Some(r.demand_ac_kwh? - h.demand.get(&r.start.timestamp())?.mean()? / 2000.)
            }));
        runs.push(json!({
            "as_of": instant(origin), "intervals": rows.len(), "soc_percentage_points": soc,
            "grid_import_kwh": grid(&h.import, true), "grid_export_kwh": grid(&h.export, false),
            "demand_ac_kwh": demand,
        }));
    }
    json!(runs)
}

pub(super) fn calculate(
    samples: &[Sample],
    cfg: &ModelSettings,
    now: i64,
    source: &Value,
) -> Value {
    let h = History::new(samples);
    let soc = source["value"].as_f64();
    let rows = horizon(&h, cfg, now, soc, true);
    let available = rows.iter().filter(|r| r.flow.is_some()).count();
    let solar_gaps = rows
        .iter()
        .filter(|r| r.fit_ac_kwh.is_none() || r.hybrid_dc_kwh.is_none())
        .count();
    let baseload = rows.iter().filter(|r| r.demand_baseload_fallback).count();
    let nighttime = rows.iter().filter(|r| r.solar_nighttime_zero).count();
    let status = match available {
        0 => "unavailable",
        n if n == rows.len() && baseload > 0 => "complete_with_baseload_fallback",
        n if n == rows.len() && solar_gaps == 0 => "complete",
        n if n == rows.len() => "complete_with_solar_gaps",
        _ => "partial",
    };
    let totals = (available == rows.len()).then(|| json!({
        "grid_import_kwh":rows.iter().filter_map(|r| r.flow.as_ref()).map(|f| f.grid_import_kwh).sum::<f64>(),
        "grid_export_kwh":rows.iter().filter_map(|r| r.flow.as_ref()).map(|f| f.grid_export_kwh).sum::<f64>(),
        "end_soc_percent":rows.last().and_then(|r| r.soc_end_percent),
    }));
    json!({
        "status": status, "as_of": instant(now), "initial_soc_percent": soc, "available_intervals": available, "intervals_without_full_solar_profile": solar_gaps,
        "initial_soc_source":source,"intervals_missing_fit_profile":rows.iter().filter(|r| r.fit_ac_kwh.is_none()).count(),"intervals_missing_hybrid_profile":rows.iter().filter(|r| r.hybrid_dc_kwh.is_none()).count(),"intervals_with_baseload_fallback":baseload,"intervals_with_nighttime_zero":nighttime,"nighttime_location_configured":cfg.projection.site.is_some(),
        "totals": totals, "intervals": rows, "backtest": backtest(&h, cfg, now),
        "assumptions": {
            "usable_kwh": cfg.usable, "reserve_percent": cfg.reserve, "conversion_and_limits": cfg.projection,
            "dispatch": "Self-consumption only. FIT AC serves demand first. Hybrid DC covers remaining demand through the inverter and can charge storage directly. Remaining FIT AC surplus can charge storage through AC-to-stored conversion. Grid supplies residual demand and receives residual PV. No scheduled grid charging, battery export, PV curtailment or export cap.",
            "solar": "Median measured power for the same London half hour, over the preceding seven elapsed days. FIT is AC and the two hybrid inputs are combined DC. Uses observed history without weather adjustment. With configured site coordinates, intervals entirely below civil twilight use an assumed zero. Twilight and mixed intervals still use a measured half-hour profile. Fully dark training slots are excluded. Missing profiles receive no solar credit; a blank solar value marks that assumption.",
            "demand": "Median measured AC demand for matching London half hour and occupancy, up to 28 days. AC balance is preferred at each timestamp. A positive direct household reading can fill a missing balance. If the matching occupancy group has fewer than three dates, the same half hour across all occupancy groups can be used and is labelled pooled. Live scenarios use the configured essential baseload in remaining demand gaps.",
            "quality": "Each profile needs three distinct dates and 90% sample coverage per half hour, with no integration across gaps over two minutes. Stale and assumed-zero reports are excluded. Initial SOC uses the latest numeric Solis state from a recent collector poll and retains the original Home Assistant report age.",
            "backtest": "Three historical 24-hour replays use earlier training bins. Mean absolute errors use paired intervals and sampled energy estimates. Live schedules can differ from the self-consumption scenario.",
        },
        "quality": {"demand_ac_balance_samples": h.demand_sources[0], "demand_direct_positive_samples": h.demand_sources[1]},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-8, "{actual} != {expected}");
    }

    #[test]
    fn dispatch_respects_ac_limits_reserve_and_dc_charge_priority() {
        let cfg = super::super::tests::cfg();
        let p = &cfg.projection;
        let reserve = cfg.usable * cfg.reserve / 100.;
        let f = dispatch(&cfg, cfg.usable, 10., 0.2, 0.5, 0.5);
        close(f.discharge_stored_kwh * p.stored_to_ac, 1.5);
        close(f.grid_import_kwh, 10. - 0.2 - 0.5 * p.hybrid_dc_to_ac - 1.5);
        let f = dispatch(&cfg, reserve + 0.25, 2., 0., 0., 0.5);
        close(f.stored_kwh, reserve);
        close(f.grid_import_kwh, 2. - 0.25 * p.stored_to_ac);
        let f = dispatch(&cfg, cfg.usable - 0.5, 0.2, 1., 0.2, 0.5);
        close(f.stored_kwh, cfg.usable);
        close(
            f.grid_export_kwh,
            0.8 - (0.5 - 0.2 * p.pv_dc_to_stored) / p.fit_ac_to_stored,
        );
        close(f.grid_import_kwh, 0.);
        let f = dispatch(&cfg, 0., 1., 0., 0., 0.5);
        close(f.discharge_stored_kwh, 0.);
        close(f.stored_kwh, 0.);
    }

    #[test]
    fn bins_require_coverage_and_replay_excludes_late_arrivals() {
        let hours = 12 * 3600;
        let start = 20000 * DAY + hours;
        let data = bins((0..4).flat_map(|day| {
            let at = start + day * DAY;
            (at..=at + SLOT).step_by(60).map(move |t| {
                let fetched = if day == 2 { start + 7 * DAY } else { t };
                (t, fetched, day as f64 * 100.)
            })
        }));
        let target = start + 6 * DAY;
        assert_eq!(
            estimate(&data, start + 3 * DAY, target, 7, None, None),
            (None, 2)
        );
        assert_eq!(
            estimate(&data, start + 4 * DAY, target, 7, None, None),
            (Some(100.), 3)
        );
        assert!(bins([(0, 0, 100.), (300, 300, 100.)]).is_empty());
    }

    #[test]
    fn nighttime_is_zero_daylight_needs_coverage_and_measured_zero_is_valid() {
        let origin = "2026-11-16T00:00:00Z"
            .parse::<DateTime<Utc>>()
            .unwrap()
            .timestamp();
        let mut cfg = super::super::tests::cfg();
        cfg.projection.site = Some((51.5, -0.1));
        let mut history = History::new(&[]);
        for day in 1..=3 {
            let at = origin + 12 * 3600 - day * DAY;
            history.fit.insert(
                at,
                Coverage {
                    seconds: SLOT,
                    watt_seconds: 0.,
                    available_at: at + SLOT,
                },
            );
            if day < 3 {
                history.hybrid.insert(
                    at,
                    Coverage {
                        seconds: SLOT,
                        watt_seconds: 120. * SLOT as f64,
                        available_at: at + SLOT,
                    },
                );
            }
        }
        let rows = horizon(&history, &cfg, origin, Some(98.), true);
        assert!(rows[0].solar_nighttime_zero);
        assert_eq!(rows[0].fit_ac_kwh, Some(0.));
        assert_eq!(rows[0].hybrid_dc_kwh, Some(0.));
        let noon = &rows[24];
        assert!(!noon.solar_nighttime_zero);
        assert_eq!(noon.fit_ac_kwh, Some(0.));
        assert_eq!(noon.fit_days, 3);
        assert_eq!(noon.hybrid_dc_kwh, None);
        assert_eq!(noon.hybrid_days, 2);
        let unconfigured = horizon(
            &History::new(&[]),
            &super::super::tests::cfg(),
            origin,
            Some(98.),
            true,
        );
        assert_eq!(unconfigured[0].fit_ac_kwh, None);
    }

    #[test]
    fn live_totals_match_rows_and_demand_only_soc_remains_separate() {
        let origin = "2026-10-25T00:10:00Z"
            .parse::<DateTime<Utc>>()
            .unwrap()
            .timestamp();
        let mut cfg = super::super::tests::cfg();
        cfg.base = 300.;
        let mut samples = Vec::new();
        for days in (1..=21).rev() {
            for at in ((origin / SLOT * SLOT - days * DAY)
                ..(origin / SLOT * SLOT - (days - 1) * DAY))
                .step_by(60)
            {
                for (metric, value) in [("fit_pv", 1800.), ("household", 300.)] {
                    samples.push(Sample {
                        at,
                        observed: at,
                        fetched: at,
                        metric: metric.into(),
                        value,
                    });
                }
            }
        }
        let result = calculate(&samples, &cfg, origin, &json!({"value": 85.}));
        let rows = result["intervals"].as_array().unwrap();
        assert_eq!(rows.len(), 49);
        assert_eq!(rows[0]["local_start"], "2026-10-25T01:10:00+01:00");
        assert_eq!(rows[2]["local_start"], "2026-10-25T01:00:00+00:00");
        close(rows[0]["demand_ac_kwh"].as_f64().unwrap(), 0.1);
        assert!(rows[0]["soc_end_percent"].as_f64().unwrap() > 85.);
        assert_eq!(
            rows.last().unwrap()["soc_end_percent"],
            result["totals"]["end_soc_percent"]
        );
        close(result["totals"]["end_soc_percent"].as_f64().unwrap(), 100.);
        for metric in ["grid_import_kwh", "grid_export_kwh"] {
            close(
                result["totals"][metric].as_f64().unwrap(),
                rows.iter()
                    .map(|r| r["flow"][metric].as_f64().unwrap())
                    .sum(),
            );
        }
        assert!(result["totals"]["grid_export_kwh"].as_f64().unwrap() > 0.);
        let demand = super::super::model_with_soc(&samples, &cfg, origin, Some(85.));
        assert!(
            demand["predictions"][0]["soc_scenario_end_percent"]
                .as_f64()
                .unwrap()
                < 85.
        );
        for row in demand["predictions"].as_array().unwrap() {
            assert!(
                row["soc_scenario_end_percent"]
                    .as_f64()
                    .is_none_or(|soc| soc <= 85.)
            );
        }
    }

    #[test]
    fn horizon_is_24_elapsed_hours_and_reports_solar_fallback_and_missing_soc() {
        let origin = "2026-10-25T00:10:00Z"
            .parse::<DateTime<Utc>>()
            .unwrap()
            .timestamp();
        let mut cfg = super::super::tests::cfg();
        let samples = [Sample {
            at: origin - 20,
            fetched: origin - 19,
            observed: origin - 301,
            metric: "soc_percent".into(),
            value: 60.,
        }];
        let result = calculate(&samples, &cfg, origin, &Value::Null);
        let rows = result["intervals"].as_array().unwrap();
        assert_eq!(rows.len(), 49);
        assert_eq!(rows.first().unwrap()["start"], json!(instant(origin)));
        assert_eq!(rows.last().unwrap()["end"], json!(instant(origin + DAY)));
        assert_eq!(result["intervals_without_full_solar_profile"], 49);
        assert!(result["initial_soc_percent"].is_null());
        assert!(result["totals"].is_null());
        assert!(rows[0]["fit_ac_kwh"].is_null());

        let mut history = History::new(&[]);
        let first = origin / SLOT * SLOT;
        for day in [7, 14, 21] {
            let time = first - day * DAY;
            history.demand.insert(
                time,
                Coverage {
                    seconds: SLOT,
                    watt_seconds: 600. * SLOT as f64,
                    available_at: time,
                },
            );
        }
        let rows = horizon(&history, &cfg, origin, Some(60.), false);
        assert!(rows[0].fit_ac_kwh.is_none());
        assert!(rows[0].hybrid_dc_kwh.is_none());
        assert!(rows[0].flow.is_some());
        assert!(rows[1].flow.is_none());

        cfg.dates.insert("2026-10-25".into(), "away".into());
        for day in 1..=10 {
            let history_start = first - day * DAY;
            for at in (history_start..history_start + DAY).step_by(SLOT as usize) {
                history.demand.insert(
                    at,
                    Coverage {
                        seconds: SLOT,
                        watt_seconds: 600. * SLOT as f64,
                        available_at: at + SLOT,
                    },
                );
            }
        }
        let rows = horizon(&history, &cfg, origin, Some(60.), false);
        assert!(rows.iter().all(|r| r.flow.is_some()));
        assert!(rows.iter().any(|r| r.demand_occupancy_pooled));
        close(rows.iter().map(|r| r.demand_ac_kwh.unwrap()).sum(), 14.4);
    }
}
