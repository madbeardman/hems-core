//! UTC observation windows for solar production, including civil twilight.
use chrono::{DateTime, Datelike, Timelike, Utc};
use std::ops::Range;

/// With coordinates, only complete minutes safely below civil twilight are
/// excluded. This also handles days with zero, one or multiple crossings.
pub(super) fn relevant_windows(range: Range<i64>, site: Option<(f64, f64)>) -> Vec<Range<i64>> {
    let Some(site) = site else {
        return std::iter::once(range).collect();
    };
    let mut windows: Vec<Range<i64>> = Vec::new();
    // Allow 0.25° for motion within a minute and rounding at the boundary.
    let cutoff = (-6.25_f64).to_radians().sin();
    for minute in (range.start.div_euclid(60) * 60..range.end).step_by(60) {
        if sine_of_elevation(minute + 30, site) >= cutoff {
            let start = minute.max(range.start);
            let end = (minute + 60).min(range.end);
            if let Some(last) = windows.last_mut().filter(|last| last.end == start) {
                last.end = end;
            } else {
                windows.push(start..end);
            }
        }
    }
    windows
}

pub(super) fn overlaps(windows: &[Range<i64>], start: i64, end: i64) -> bool {
    let i = windows.partition_point(|window| window.end <= start);
    windows.get(i).is_some_and(|window| window.start < end)
}

// NOAA fractional-year solar position approximation.
fn sine_of_elevation(at: i64, (lat, lon): (f64, f64)) -> f64 {
    let t = DateTime::<Utc>::from_timestamp(at, 0).unwrap();
    let hour = t.num_seconds_from_midnight() as f64 / 3600.;
    let year_days = if t.with_ordinal(366).is_some() {
        366.
    } else {
        365.
    };
    let angle = std::f64::consts::TAU / year_days * (t.ordinal0() as f64 + (hour - 12.) / 24.);
    let (sin, cos) = angle.sin_cos();
    let (sin2, cos2) = (2. * angle).sin_cos();
    let (sin3, cos3) = (3. * angle).sin_cos();
    let minutes =
        229.18 * (0.000075 + 0.001868 * cos - 0.032077 * sin - 0.014615 * cos2 - 0.040849 * sin2);
    let declination = 0.006918 - 0.399912 * cos + 0.070257 * sin - 0.006758 * cos2
        + 0.000907 * sin2
        - 0.002697 * cos3
        + 0.00148 * sin3;
    let hour_angle = (hour * 15. + minutes / 4. + lon - 180.).to_radians();
    let latitude = lat.to_radians();
    latitude.sin() * declination.sin() + latitude.cos() * declination.cos() * hour_angle.cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp(date: &str) -> i64 {
        date.parse::<DateTime<Utc>>().unwrap().timestamp()
    }

    #[test]
    fn polar_night_midnight_sun_and_unknown_site() {
        let summer = timestamp("2026-06-21T00:00:00Z");
        let winter = timestamp("2026-12-21T00:00:00Z");
        let polar = Some((90., 0.));
        assert!(relevant_windows(winter..winter + 86400, polar).is_empty());
        for (start, site) in [(summer, polar), (winter, None)] {
            let windows = relevant_windows(start..start + 86400, site);
            assert_eq!(windows.len(), 1);
            assert_eq!(windows[0], start..start + 86400);
        }
    }

    #[test]
    fn twilight_crossing_survives_partial_slots() {
        let start = timestamp("2026-11-16T00:00:00Z");
        let site = Some((51.5, -0.1));
        let windows = relevant_windows(start..start + 86400, site);
        assert_eq!(windows.len(), 1);
        let w = &windows[0];
        assert!(overlaps(&windows, w.start - 1, w.start + 1));
        assert!(overlaps(&windows, w.end - 1, w.end + 1));
        assert!(!overlaps(&windows, w.start - 60, w.start));
        assert!(!overlaps(&windows, w.end, w.end + 60));
        let just_before_sunrise = timestamp("2026-11-16T07:00:00Z");
        assert!(sine_of_elevation(just_before_sunrise, site.unwrap()) < 0.);
        assert!(overlaps(
            &windows,
            just_before_sunrise,
            just_before_sunrise + 1
        ));
    }
}
