//! Today dashboard — the default brainrot view. Cyberpunk-branded layout
//! that mirrors the original ANSI design but with ccft chrome (header bar,
//! magenta caret section markers, subtle separators).

use crate::brainrot::aggregate::*;
use crate::ledger_read::{
    compute_coverage, iter_records, load_state_events, now_secs, parse_range, percentile, Coverage,
};
use crate::theme::*;

pub fn run(spec: &str) -> Result<(), Box<dyn std::error::Error>> {
    let range = parse_range(spec).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let a = Aggregate::ingest(iter_records(Some(range.since), Some(range.until)));
    let baseline_records: Vec<_> = iter_records(None, None).collect();
    let baseline = Baseline::from_records(&baseline_records);
    let cov = compute_coverage(&load_state_events(), range.since, range.until);

    header("brainrot", &range.label);

    if a.n == 0 {
        empty_view(&cov);
        return Ok(());
    }

    scores_view(&a, &baseline, &cov);
    summary_view(&a);
    burn_view(&a);
    by_hour_view(&a);
    models_view(&a);
    streak_view(&a);
    peak_warning(&a);

    println!();
    Ok(())
}

fn empty_view(cov: &Coverage) {
    if cov.currently_off {
        bullet(&red("⚠ ledger is OFF, no records being captured"));
        if let Some(ts) = cov.last_event_ts {
            let ago = now_secs() - ts;
            println!(
                "       {}",
                dim(&format!("turned off {} ago", fmt_dur(ago)))
            );
        }
    } else if !cov.off_intervals.is_empty() {
        let off_total: f64 = cov.off_intervals.iter().map(|(a, b)| b - a).sum();
        bullet(&dim(&format!(
            "(no records, ledger was off for {} of {})",
            fmt_dur(off_total),
            fmt_dur(cov.total_s)
        )));
    } else {
        bullet(&dim("(no records, go make some API calls)"));
    }
    println!();
}

fn scores_view(a: &Aggregate, baseline: &Baseline, cov: &Coverage) {
    let bot = bot_score(a, baseline);
    let drv = driver_score(a, baseline);
    let drv_bootstrap = driver_is_bootstrapping(baseline);
    section("vibe");
    println!(
        "    {}     {} / 100   {} {}",
        bold("bot"),
        score_color(bot, &format!("{:>3}", bot)),
        grey("—"),
        vibe_label(bot)
    );
    if drv_bootstrap {
        println!(
            "    {}  {} / 100   {} {}",
            bold("driver"),
            grey("  —"),
            grey("—"),
            grey("learning your driving baseline (need a few more driven messages)")
        );
    } else {
        println!(
            "    {}  {} / 100   {} {}",
            bold("driver"),
            score_color(drv, &format!("{:>3}", drv)),
            grey("—"),
            vibe_label(drv)
        );
    }
    if !drv_bootstrap {
        if let Some(diag) = diagnosis_for(&a, &baseline) {
            println!("        {} {}", grey("↳"), subtle(diag));
        }
    }
    if let Some(line) = render_coverage_line(cov) {
        println!("        {} {}", grey("↳"), line);
    }
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

fn render_coverage_line(cov: &Coverage) -> Option<String> {
    let total = if cov.total_s > 0.0 { cov.total_s } else { 1.0 };
    let pct = cov.active_s / total * 100.0;
    let n_gaps = cov.off_intervals.len();

    if cov.currently_off {
        if let Some(ts) = cov.last_event_ts {
            let ago = now_secs() - ts;
            return Some(red(&format!("⚠ ledger OFF ({} ago)", fmt_dur(ago))));
        }
        return Some(red("⚠ ledger OFF"));
    }
    if n_gaps == 0 && pct > 99.9 {
        return None;
    }
    let word = if n_gaps == 1 { "gap" } else { "gaps" };
    let s = format!(
        "coverage {} of {} ({:.0}%, {} {})",
        fmt_dur(cov.active_s),
        fmt_dur(total),
        pct,
        n_gaps,
        word
    );
    Some(if pct > 95.0 {
        green(&s)
    } else if pct > 70.0 {
        yellow(&s)
    } else {
        red(&s)
    })
}

fn summary_view(a: &Aggregate) {
    let avg_lat = a.lat_sum as f64 / a.n.max(1) as f64;
    let mut lats = a.lats.clone();
    let p50 = percentile(&mut lats.clone(), 50.0);
    let p99 = percentile(&mut lats, 99.0);
    let span = a.last_ts.unwrap_or(0.0) - a.first_ts.unwrap_or(0.0);
    let sess_count = a.sessions.len();
    let sess_word = if sess_count == 1 {
        "session"
    } else {
        "sessions"
    };

    section("traffic");
    println!(
        "    {}     {}  {} {}  {} {} {}",
        bold("reqs"),
        bold(&a.n.to_string()),
        dim("over"),
        fmt_dur(span),
        grey("·"),
        bold(&sess_count.to_string()),
        sess_word,
    );
    println!(
        "    {}   {} in  {}  {} out  {}  {} total",
        bold("tokens"),
        cyan(&fmt_n(a.r#in)),
        grey("·"),
        cyan(&fmt_n(a.out)),
        grey("·"),
        cyan(&fmt_n(a.tot))
    );
    println!(
        "    {}  p50 {}ms  {}  p99 {}  {}  avg {}ms",
        bold("latency"),
        p50 as u64,
        grey("·"),
        heat_ms(p99, &format!("{}ms", p99 as u64)),
        grey("·"),
        avg_lat as u64
    );
}

fn burn_view(a: &Aggregate) {
    if a.by_minute.is_empty() {
        return;
    }
    let mut keys: Vec<i64> = a.by_minute.keys().copied().collect();
    keys.sort();
    let first_min = keys[0];
    let last_min = *keys.last().unwrap();
    let width = ((last_min - first_min + 1) as usize).clamp(10, 60);
    let mut series = Vec::new();
    for m in first_min..=last_min {
        let v = a.by_minute.get(&m).map(|b| b.tot).unwrap_or(0);
        series.push(v as f64);
    }
    let spark = sparkline(&series, Some(width));
    let peak_idx = series
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let peak_min = first_min + peak_idx as i64;
    let peak_dt = time::OffsetDateTime::from_unix_timestamp(peak_min * 60)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let peak_local = peak_dt.to_offset(local_offset);
    let peak_str = format!("{:02}:{:02}", peak_local.hour(), peak_local.minute());
    let peak_max = series.iter().cloned().fold(0.0_f64, f64::max);

    section("burn");
    println!("    {}", cyan(&spark));
    println!(
        "    {} {} {} {} tok/min",
        dim("peak"),
        magenta(&peak_str),
        grey("·"),
        bold(&fmt_n(peak_max as u64)),
    );
}

fn by_hour_view(a: &Aggregate) {
    if a.by_hour.is_empty() {
        return;
    }
    section("by hour");
    println!("    {}", grey("00    06    12    18  23"));
    let mut line = String::from("    ");
    for h in 0u8..24 {
        let cell = match a.by_hour.get(&h) {
            Some(hb) if hb.n > 0 => {
                let avg_lat = hb.lat_sum as f64 / hb.n as f64;
                let intensity =
                    ((hb.n as f64 / (a.n as f64 / 24.0).max(1.0)) * 4.0).min(7.0) as usize;
                let ch = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'][intensity];
                heat_ms(avg_lat, &ch.to_string())
            }
            _ => dim("·"),
        };
        line.push_str(&cell);
    }
    println!("{}", line);
}

fn models_view(a: &Aggregate) {
    if a.models.is_empty() {
        return;
    }
    section("models");
    let total: u64 = a.models.values().sum();
    let mut by_count: Vec<(&String, &u64)> = a.models.iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(a.1));
    for (model, count) in by_count.iter().take(5) {
        let pct = **count as f64 / total as f64;
        let bar_w = 40usize;
        let filled = (pct * bar_w as f64) as usize;
        let bar_str = bar(filled, bar_w);
        let label = short_model(model);
        println!(
            "    {:14}  {}  {} ({})",
            label,
            cyan(&bar_str),
            dim(&format!("{:.0}%", pct * 100.0)),
            count
        );
    }
}

fn streak_view(a: &Aggregate) {
    let now = now_secs();
    let recent_30: usize = a.records.iter().filter(|r| r.ts > now - 1800.0).count();
    if recent_30 > 0 {
        section("streak");
        println!(
            "    {}  {} reqs in last 30min",
            magenta("🔥"),
            bold(&recent_30.to_string())
        );
    } else if let Some(last) = a.last_ts {
        if now - last < 86400.0 {
            section("streak");
            println!("    {}  idle {}", grey("💤"), dim(&fmt_dur(now - last)));
        }
    }
}

fn peak_warning(a: &Aggregate) {
    if a.by_hour.is_empty() {
        return;
    }
    let peak_hour = a
        .by_hour
        .iter()
        .max_by_key(|(_, hb)| hb.n)
        .map(|(h, _)| *h)
        .unwrap_or(0);
    let slow_hour = a
        .by_hour
        .iter()
        .max_by(|a, b| {
            let la = a.1.lat_sum as f64 / a.1.n.max(1) as f64;
            let lb = b.1.lat_sum as f64 / b.1.n.max(1) as f64;
            la.partial_cmp(&lb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(h, _)| *h)
        .unwrap_or(0);

    if peak_hour == slow_hour {
        println!();
        println!(
            "  {}  {}",
            yellow("⚠"),
            dim(&format!(
                "your peak hour ({:02}:00) is also your slowest. you're choking the model.",
                peak_hour
            ))
        );
    }
}
