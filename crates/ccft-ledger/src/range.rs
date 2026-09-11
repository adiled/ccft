//! Human time ranges + percentile. Provider-agnostic, pure.

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct Range {
    pub since: f64,
    pub until: f64,
    pub label: String,
}

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn today_start() -> f64 {
    let now = now_secs() as i64;
    let dt =
        time::OffsetDateTime::from_unix_timestamp(now).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let local = dt.to_offset(local_offset);
    let midnight = local.replace_time(time::Time::MIDNIGHT);
    midnight.unix_timestamp() as f64
}

/// Parse a human time-range spec into `[since, until]` epoch seconds.
///
/// Accepts: `today`, `yesterday`, `week`/`7d`, `this-week`, `24h`, `prev 7d`,
/// `all`, `Nh`, `Nd`, `Nw`, `Nmo`, `YYYY-MM-DD`, or empty (→ today).
pub fn parse_range(spec: &str) -> Result<Range, String> {
    let now = now_secs();
    let today = today_start();
    let s = spec.trim().to_lowercase();

    if s.is_empty() || s == "today" {
        return Ok(Range {
            since: today,
            until: now,
            label: "today".into(),
        });
    }
    if s == "yesterday" {
        return Ok(Range {
            since: today - 86400.0,
            until: today,
            label: "yesterday".into(),
        });
    }
    if s == "week" || s == "7d" {
        return Ok(Range {
            since: now - 7.0 * 86400.0,
            until: now,
            label: "last 7d".into(),
        });
    }
    if s == "this-week" {
        let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
        let now_dt = time::OffsetDateTime::from_unix_timestamp(now as i64)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
            .to_offset(local_offset);
        let days_since_monday = (now_dt.weekday().number_from_monday() - 1) as i64;
        let monday_dt = now_dt - time::Duration::days(days_since_monday);
        let monday_midnight = monday_dt.replace_time(time::Time::MIDNIGHT);
        let since = monday_midnight.unix_timestamp() as f64;
        let until = since + 7.0 * 86400.0 - 1.0;
        return Ok(Range {
            since,
            until,
            label: "this week".into(),
        });
    }
    if s == "24h" {
        return Ok(Range {
            since: now - 86400.0,
            until: now,
            label: "last 24h".into(),
        });
    }
    if s == "prev 7d" {
        return Ok(Range {
            since: now - 14.0 * 86400.0,
            until: now - 7.0 * 86400.0,
            label: "prev 7d".into(),
        });
    }
    if s == "all" {
        return Ok(Range {
            since: 0.0,
            until: now,
            label: "all-time".into(),
        });
    }
    if let Some(stripped) = s.strip_suffix('h') {
        if let Ok(n) = stripped.parse::<u64>() {
            return Ok(Range {
                since: now - n as f64 * 3600.0,
                until: now,
                label: format!("last {}h", n),
            });
        }
    }
    if let Some(stripped) = s.strip_suffix('d') {
        if let Ok(n) = stripped.parse::<u64>() {
            return Ok(Range {
                since: now - n as f64 * 86400.0,
                until: now,
                label: format!("last {}d", n),
            });
        }
    }
    if let Some(stripped) = s.strip_suffix('w') {
        if let Ok(n) = stripped.parse::<u64>() {
            return Ok(Range {
                since: now - n as f64 * 7.0 * 86400.0,
                until: now,
                label: format!("last {}w", n),
            });
        }
    }
    if let Some(stripped) = s.strip_suffix("mo") {
        if let Ok(n) = stripped.parse::<u64>() {
            return Ok(Range {
                since: now - n as f64 * 30.0 * 86400.0,
                until: now,
                label: format!("last {}mo", n),
            });
        }
    }
    if s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        let fmt = time::format_description::parse("[year]-[month]-[day]")
            .map_err(|e| e.to_string())?;
        let date = time::Date::parse(&s, &fmt).map_err(|e| e.to_string())?;
        let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
        let dt = date
            .with_time(time::Time::MIDNIGHT)
            .assume_offset(local_offset);
        let start = dt.unix_timestamp() as f64;
        return Ok(Range {
            since: start,
            until: start + 86400.0,
            label: s,
        });
    }
    Err(format!(
        "don't understand range '{}'. Try: today, yesterday, 24h, 7d, all, Nh, Nd, YYYY-MM-DD",
        spec
    ))
}

/// p-th percentile of a `u64` slice (linear interpolation, in-place sort).
pub fn percentile(values: &mut [u64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable();
    let k = (values.len() - 1) as f64 * p / 100.0;
    let f = k.floor() as usize;
    let c = (f + 1).min(values.len() - 1);
    let lo = values[f] as f64;
    let hi = values[c] as f64;
    lo + (hi - lo) * (k - f as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_specs_parse() {
        for spec in ["today", "24h", "7d", "2h", "all", "prev 7d", "2024-01-15"] {
            let r = parse_range(spec).unwrap();
            assert!(r.since <= r.until, "{spec}: since={} until={}", r.since, r.until);
        }
    }

    #[test]
    fn percentile_interpolates() {
        let mut v = vec![10, 20, 30, 40, 50];
        assert_eq!(percentile(&mut v, 50.0), 30.0);
        assert!(percentile(&mut v, 0.0).abs() - 10.0 < 1e-9);
        let empty: Vec<u64> = vec![];
        assert_eq!(percentile(&mut empty.clone(), 50.0), 0.0);
    }
}
