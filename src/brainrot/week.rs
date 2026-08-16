//! 7-day rollup. Aggregates by day, plus day-of-week pattern and peak hour.

use crate::brainrot::aggregate::*;
use crate::ledger_read::{
    compute_coverage, iter_records, load_state_events, parse_range, percentile,
};
use crate::theme::*;
use std::collections::HashMap;

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let range = parse_range("7d").map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let records: Vec<_> = iter_records(Some(range.since), Some(range.until)).collect();
    let cov = compute_coverage(&load_state_events(), range.since, range.until);

    header("brainrot", &range.label);
    if records.is_empty() {
        if cov.currently_off {
            bullet(&red("⚠ ledger is OFF, no records being captured"));
        } else {
            bullet(&dim("(no records this week)"));
        }
        println!();
        return Ok(());
    }

    let a = Aggregate::ingest(records.iter().cloned());
    let baseline_records: Vec<_> = iter_records(None, None).collect();
    let baseline = Baseline::from_records(&baseline_records);

    // Scores
    let bot = bot_score(&a, &baseline);
    let drv = driver_score(&a, &baseline);
    section("vibe");
    println!(
        "    {}     {} / 100   {} {}",
        bold("bot"),
        score_color(bot, &format!("{:>3}", bot)),
        grey("—"),
        vibe_label(bot)
    );
    println!(
        "    {}  {} / 100   {} {}",
        bold("driver"),
        score_color(drv, &format!("{:>3}", drv)),
        grey("—"),
        vibe_label(drv)
    );
    if let Some(d) = diagnosis(&score_breakdown(&a, &baseline)) {
        println!("        {} {}", grey("↳"), subtle(d));
    }

    // Totals
    section("totals");
    println!(
        "    {}     {}  {}  {} tokens  {}  {} sessions",
        bold("reqs"),
        bold(&a.n.to_string()),
        grey("·"),
        cyan(&fmt_n(a.tot)),
        grey("·"),
        bold(&a.sessions.len().to_string()),
    );

    // by_day rollup
    let mut by_day: HashMap<String, DayBucket> = HashMap::new();
    let local = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    for r in &records {
        let dt = time::OffsetDateTime::from_unix_timestamp(r.ts as i64)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
            .to_offset(local);
        let day = format!(
            "{:04}-{:02}-{:02}",
            dt.year(),
            u8::from(dt.month()),
            dt.day()
        );
        let bucket = by_day.entry(day).or_default();
        bucket.n += 1;
        bucket.tot += r.tot;
        bucket.lat_sum += r.lat;
        bucket.weekday = dt.weekday().number_from_monday();
    }
    let mut days: Vec<(String, DayBucket)> = by_day.into_iter().collect();
    days.sort_by(|a, b| a.0.cmp(&b.0));
    let max_tot: u64 = days.iter().map(|(_, d)| d.tot).max().unwrap_or(1);

    section("daily");
    for (day, d) in &days {
        let dt = time::Date::parse(
            day,
            &time::format_description::parse("[year]-[month]-[day]").unwrap(),
        )
        .unwrap_or(time::Date::MIN);
        let dow = match dt.weekday() {
            time::Weekday::Monday => "Mon",
            time::Weekday::Tuesday => "Tue",
            time::Weekday::Wednesday => "Wed",
            time::Weekday::Thursday => "Thu",
            time::Weekday::Friday => "Fri",
            time::Weekday::Saturday => "Sat",
            time::Weekday::Sunday => "Sun",
        };
        let bar_w = 40usize;
        let filled = ((d.tot as f64 / max_tot as f64) * bar_w as f64) as usize;
        let avg_lat = if d.n > 0 { d.lat_sum / d.n } else { 0 };
        let lat_str = format!("{}ms", avg_lat);
        println!(
            "    {} {}  {}  {:>7}  {} req  {}",
            dow,
            &day[5..],
            cyan(&bar(filled, bar_w)),
            fmt_n(d.tot),
            dim(&d.n.to_string()),
            heat_ms(avg_lat as f64, &lat_str),
        );
    }

    // Day-of-week pattern
    let mut dow_count: HashMap<u8, u64> = HashMap::new();
    for (_, d) in &days {
        *dow_count.entry(d.weekday).or_insert(0) += d.n;
    }
    let max_dow: u64 = dow_count.values().copied().max().unwrap_or(1);
    section("day pattern");
    let labels = ['M', 'T', 'W', 'T', 'F', 'S', 'S'];
    let mut line = String::from("    ");
    let sparkchars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    for (i, lbl) in labels.iter().enumerate() {
        let n = dow_count.get(&((i + 1) as u8)).copied().unwrap_or(0);
        let intensity = ((n as f64 / max_dow as f64) * 7.0).min(7.0) as usize;
        line.push(*lbl);
        line.push(sparkchars[intensity]);
        line.push(' ');
    }
    println!("{}", line);

    // Peak hour vs slow hour
    if !a.by_hour.is_empty() {
        let peak = a.by_hour.iter().max_by_key(|(_, b)| b.n).unwrap();
        let slow = a
            .by_hour
            .iter()
            .max_by(|x, y| {
                let lx = x.1.lat_sum as f64 / x.1.n.max(1) as f64;
                let ly = y.1.lat_sum as f64 / y.1.n.max(1) as f64;
                lx.partial_cmp(&ly).unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap();
        section("peaks");
        println!("    busiest hour:  {:02}:00  ({} reqs)", peak.0, peak.1.n);
        let slow_avg = slow.1.lat_sum as f64 / slow.1.n.max(1) as f64;
        let slow_str = format!("{}ms avg", slow_avg as u64);
        println!(
            "    slowest hour:  {:02}:00  ({})",
            slow.0,
            heat_ms(slow_avg, &slow_str)
        );
    }

    // Models
    if !a.models.is_empty() {
        section("models");
        let total: u64 = a.models.values().sum();
        let mut sorted: Vec<(&String, &u64)> = a.models.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        for (m, c) in sorted.iter().take(5) {
            let pct = **c as f64 / total as f64;
            let bar_w = 40usize;
            let filled = (pct * bar_w as f64) as usize;
            println!(
                "    {:14}  {}  {} ({})",
                short_model(m),
                cyan(&bar(filled, bar_w)),
                dim(&format!("{:.0}%", pct * 100.0)),
                c
            );
        }
    }

    let _ = percentile; // silence unused if minimal-build
    println!();
    Ok(())
}

#[derive(Default)]
struct DayBucket {
    n: u64,
    tot: u64,
    lat_sum: u64,
    weekday: u8, // 1 = Mon ... 7 = Sun
}

fn score_color(score: u32, s: &str) -> String {
    if score < 40 {
        green(s)
    } else if score < 70 {
        yellow(s)
    } else {
        red(s)
    }
}
