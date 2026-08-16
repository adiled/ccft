//! session — list today's sessions, or drill into one by sid prefix.

use crate::brainrot::aggregate::*;
use crate::ledger_read::{iter_records, now_secs, parse_range, Record};
use crate::theme::*;
use std::collections::HashMap;

pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return list_today();
    }
    drill_in(&args[0])
}

#[derive(Default)]
struct SessionBucket {
    n: u64,
    tot: u64,
    first: f64,
    last: f64,
    lat_sum: u64,
    models: HashMap<String, u64>,
}

fn list_today() -> Result<(), Box<dyn std::error::Error>> {
    let range = parse_range("today").map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let mut sessions: HashMap<String, SessionBucket> = HashMap::new();
    for r in iter_records(Some(range.since), Some(range.until)) {
        let key = r.sid.clone().unwrap_or_else(|| "(no-sid)".into());
        let s = sessions.entry(key).or_default();
        s.n += 1;
        s.tot += r.tot;
        s.lat_sum += r.lat;
        if s.first == 0.0 || r.ts < s.first {
            s.first = r.ts;
        }
        if r.ts > s.last {
            s.last = r.ts;
        }
        let m = r.model.unwrap_or_else(|| "?".into());
        *s.models.entry(m).or_insert(0) += 1;
    }

    if sessions.is_empty() {
        header("brainrot sessions", "today");
        bullet(&dim("no sessions today"));
        println!();
        return Ok(());
    }

    header("brainrot sessions", &format!("today  ({})", sessions.len()));
    println!(
        "  {:10}  {:>5}  {:>8}  {:>9}  {:>7}  model",
        bold("sid"),
        bold("reqs"),
        bold("tokens"),
        bold("avg lat"),
        bold("span")
    );
    let mut ordered: Vec<(String, SessionBucket)> = sessions.into_iter().collect();
    ordered.sort_by(|a, b| b.1.n.cmp(&a.1.n));
    for (sid, s) in ordered {
        let sid_short: String = if sid == "(no-sid)" {
            sid.clone()
        } else {
            sid.chars().take(8).collect()
        };
        let avg_lat = if s.n > 0 { s.lat_sum / s.n } else { 0 };
        let span = fmt_dur(s.last - s.first);
        let lat_str = format!("{}ms", avg_lat);
        let top_model = s
            .models
            .iter()
            .max_by_key(|(_, c)| **c)
            .map(|(m, _)| short_model(m))
            .unwrap_or_else(|| "?".into());
        println!(
            "  {:10}  {:>5}  {:>8}  {:>9}  {:>7}  {}",
            sid_short,
            s.n,
            fmt_n(s.tot),
            heat_ms(avg_lat as f64, &lat_str),
            span,
            dim(&top_model)
        );
    }
    println!();
    Ok(())
}

fn drill_in(query: &str) -> Result<(), Box<dyn std::error::Error>> {
    let now = now_secs();
    let matched: Vec<Record> = iter_records(Some(0.0), Some(now))
        .filter(|r| {
            r.sid
                .as_deref()
                .map(|s| s.starts_with(query))
                .unwrap_or(false)
        })
        .collect();
    if matched.is_empty() {
        println!("\n  {}\n", dim(&format!("no session matching '{}'", query)));
        return Ok(());
    }
    let full_sid = matched[0].sid.clone().unwrap_or_default();
    let a = Aggregate::ingest(matched.iter().cloned());
    let span = a.last_ts.unwrap_or(0.0) - a.first_ts.unwrap_or(0.0);
    let api_time = a.lat_sum as f64 / 1000.0;
    let pct_in_api = api_time / span.max(1.0) * 100.0;

    header("brainrot session", &full_sid);

    println!("  {}      {}", bold("reqs"), bold(&a.n.to_string()));
    println!(
        "  {}      {} wall  {}  {} in API  {}",
        bold("span"),
        fmt_dur(span),
        grey("·"),
        fmt_dur(api_time),
        dim(&format!("({:.0}%)", pct_in_api))
    );
    println!(
        "  {}    {} in  {}  {} out",
        bold("tokens"),
        cyan(&fmt_n(a.r#in)),
        grey("·"),
        cyan(&fmt_n(a.out))
    );
    let top_model = a
        .models
        .iter()
        .max_by_key(|(_, c)| **c)
        .map(|(m, _)| short_model(m))
        .unwrap_or_else(|| "?".into());
    println!("  {}     {}", bold("model"), top_model);

    println!();
    println!("  {}", grey("timeline:"));
    let local = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    for (i, r) in a.records.iter().enumerate() {
        let dt = time::OffsetDateTime::from_unix_timestamp(r.ts as i64)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
            .to_offset(local);
        let when = format!("{:02}:{:02}:{:02}", dt.hour(), dt.minute(), dt.second());
        let mut gap_str = String::new();
        if i > 0 {
            let g = r.ts - a.records[i - 1].ts;
            if g > 30.0 {
                gap_str = dim(&format!("  +{} thinking", fmt_dur(g)));
            }
        }
        let lat_str = format!("{}ms", r.lat);
        println!(
            "  {}  in:{:>6}  out:{:>6}  lat:{}{}",
            dim(&when),
            fmt_n(r.r#in),
            fmt_n(r.out),
            heat_ms(r.lat as f64, &lat_str),
            gap_str
        );
    }
    println!();
    Ok(())
}
