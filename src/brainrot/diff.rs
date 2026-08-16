//! diff A B — compare two ranges. Defaults to today vs yesterday.

use crate::brainrot::aggregate::*;
use crate::ledger_read::{iter_records, parse_range, percentile, Range};
use crate::theme::*;

pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let (sa, sb): (String, String) = match args.len() {
        0 | 1 => ("today".into(), "yesterday".into()),
        _ => (args[0].clone(), args[1].clone()),
    };
    let ra = parse_range(&sa).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let rb = parse_range(&sb).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let a = Aggregate::ingest(iter_records(Some(ra.since), Some(ra.until)));
    let b = Aggregate::ingest(iter_records(Some(rb.since), Some(rb.until)));
    let baseline_records: Vec<_> = iter_records(None, None).collect();
    let baseline = Baseline::from_records(&baseline_records);

    header("brainrot diff", &format!("{}  vs  {}", ra.label, rb.label));

    if a.n == 0 && b.n == 0 {
        bullet(&dim("(no records in either period)"));
        println!();
        return Ok(());
    }

    let bot_a = bot_score(&a, &baseline) as i64;
    let bot_b = bot_score(&b, &baseline) as i64;
    let drv_a = driver_score(&a, &baseline) as i64;
    let drv_b = driver_score(&b, &baseline) as i64;
    let avg_lat_a = if a.n > 0 {
        a.lat_sum as f64 / a.n as f64
    } else {
        0.0
    };
    let avg_lat_b = if b.n > 0 {
        b.lat_sum as f64 / b.n as f64
    } else {
        0.0
    };
    let avg_in_a = if a.n > 0 {
        a.r#in as f64 / a.n as f64
    } else {
        0.0
    };
    let avg_in_b = if b.n > 0 {
        b.r#in as f64 / b.n as f64
    } else {
        0.0
    };
    let avg_out_a = if a.n > 0 {
        a.out as f64 / a.n as f64
    } else {
        0.0
    };
    let avg_out_b = if b.n > 0 {
        b.out as f64 / b.n as f64
    } else {
        0.0
    };
    let p99_a = percentile(&mut a.lats.clone(), 99.0);
    let p99_b = percentile(&mut b.lats.clone(), 99.0);
    let ratio_a = if a.r#in > 0 {
        a.out as f64 / a.r#in as f64
    } else {
        0.0
    };
    let ratio_b = if b.r#in > 0 {
        b.out as f64 / b.r#in as f64
    } else {
        0.0
    };

    let rows: Vec<(&str, String, String, (String, fn(&str) -> String))> = vec![
        (
            "bot score",
            format!("{}/100", bot_a),
            format!("{}/100", bot_b),
            delta(bot_a as f64, bot_b as f64, true),
        ),
        (
            "driver",
            format!("{}/100", drv_a),
            format!("{}/100", drv_b),
            delta(drv_a as f64, drv_b as f64, true),
        ),
        (
            "requests",
            a.n.to_string(),
            b.n.to_string(),
            delta(a.n as f64, b.n as f64, false),
        ),
        (
            "total tok",
            fmt_n(a.tot),
            fmt_n(b.tot),
            delta(a.tot as f64, b.tot as f64, false),
        ),
        (
            "avg in",
            fmt_n(avg_in_a as u64),
            fmt_n(avg_in_b as u64),
            delta(avg_in_a, avg_in_b, true),
        ),
        (
            "avg out",
            fmt_n(avg_out_a as u64),
            fmt_n(avg_out_b as u64),
            delta(avg_out_a, avg_out_b, false),
        ),
        (
            "avg lat",
            format!("{}ms", avg_lat_a as u64),
            format!("{}ms", avg_lat_b as u64),
            delta(avg_lat_a, avg_lat_b, true),
        ),
        (
            "p99 lat",
            format!("{}ms", p99_a as u64),
            format!("{}ms", p99_b as u64),
            delta(p99_a, p99_b, true),
        ),
        (
            "out/in",
            format!("{:.2}", ratio_a),
            format!("{:.2}", ratio_b),
            delta(ratio_a, ratio_b, false),
        ),
        (
            "sessions",
            a.sessions.len().to_string(),
            b.sessions.len().to_string(),
            delta(a.sessions.len() as f64, b.sessions.len() as f64, false),
        ),
    ];

    println!();
    println!(
        "  {:12} {:>14}  {:>14}    drift",
        bold("metric"),
        bold(&ra.label),
        bold(&rb.label)
    );
    println!("  {}", grey(&"─".repeat(64)));
    for (label, va, vb, (d, col)) in rows {
        println!("  {:12} {:>14}  {:>14}    {}", label, va, vb, col(&d));
    }

    if let Some(diag) = diagnosis(&score_breakdown(&a, &baseline)) {
        println!();
        println!("  {:>14}  {} {}", ra.label, grey("↳"), subtle(diag));
    }
    if let Some(diag) = diagnosis(&score_breakdown(&b, &baseline)) {
        println!("  {:>14}  {} {}", rb.label, grey("↳"), subtle(diag));
    }

    // Model mix shift
    section("model mix shift");
    let mut all_models: Vec<&String> = a.models.keys().chain(b.models.keys()).collect();
    all_models.sort();
    all_models.dedup();
    let tot_a = a.models.values().sum::<u64>().max(1) as f64;
    let tot_b = b.models.values().sum::<u64>().max(1) as f64;
    for m in all_models {
        let pa = *a.models.get(m).unwrap_or(&0) as f64 / tot_a * 100.0;
        let pb = *b.models.get(m).unwrap_or(&0) as f64 / tot_b * 100.0;
        let diff_pp = pa - pb;
        let sign = if diff_pp >= 0.0 { "+" } else { "" };
        let col: fn(&str) -> String = if diff_pp.abs() < 5.0 {
            green
        } else if diff_pp.abs() < 15.0 {
            yellow
        } else {
            red
        };
        let diff_str = format!("{}{:.1}pp", sign, diff_pp);
        println!(
            "    {:14}  {:>5.1}%  vs  {:>5.1}%   {}",
            short_model(m),
            pa,
            pb,
            col(&diff_str)
        );
    }

    println!();
    Ok(())
}

fn delta(av: f64, bv: f64, lower_is_better: bool) -> (String, fn(&str) -> String) {
    if bv == 0.0 {
        return ("—".into(), grey);
    }
    let pct = (av - bv) / bv * 100.0;
    let sign = if pct >= 0.0 { "+" } else { "" };
    let good = if lower_is_better {
        pct < 0.0
    } else {
        pct > 0.0
    };
    let col: fn(&str) -> String = if good {
        green
    } else if pct.abs() > 5.0 {
        red
    } else {
        yellow
    };
    (format!("{}{:.0}%", sign, pct), col)
}

#[allow(dead_code)]
fn _silence(_r: Range) {}
