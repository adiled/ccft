//! Driver vs bot turn split by inter-arrival gap. Learned log-normal model by default; --det forces 5s rule.

use crate::brainrot::aggregate::{
    classify_turns, classify_turns_prob_with_model, split_summary, TurnKind,
};
use crate::ledger_read::{compute_coverage, iter_records, load_state_events, parse_range, Record};
use crate::theme::*;
use std::collections::HashMap;

pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let use_det = args.iter().any(|a| *a == "--det" || *a == "-d");
    let spec = if use_det {
        args.iter()
            .filter(|a| *a != "--det" && *a != "-d")
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        args.join(" ")
    };
    let range = parse_range(&spec).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let records: Vec<Record> = iter_records(Some(range.since), Some(range.until)).collect();
    let cov = compute_coverage(&load_state_events(), range.since, range.until);

    header("brainrot split", &range.label);

    if cov.currently_off {
        bullet(&red("⚠ ledger is OFF, no records being captured"));
        println!();
        return Ok(());
    }
    if records.is_empty() {
        if !cov.off_intervals.is_empty() {
            let off_s: f64 = cov.off_intervals.iter().map(|(a, b)| b - a).sum();
            bullet(&dim(&format!(
                "(no records, ledger was off for {} of {})",
                fmt_dur(off_s),
                fmt_dur(cov.total_s)
            )));
        } else {
            bullet(&dim("(no records in range)"));
        }
        println!();
        return Ok(());
    }

    let (kinds, mix) = if use_det {
        (classify_turns(&records), None)
    } else {
        classify_turns_prob_with_model(&records)
    };
    if let Some(m) = &mix {
        bullet(&dim(&format!(
            "learned gap model: bot-loop ~{} · human turn ~{} · boundary ~{} ({} gaps, {} EM iters, ll {:.1})",
            fmt_dur(m.mu_bot.exp()),
            fmt_dur(m.mu_drv.exp()),
            fmt_dur(m.threshold_sec),
            m.n_gaps,
            m.iterations,
            m.log_lik
        )));
    } else if !records.is_empty() && !use_det {
        bullet(&dim(
            "no stable 2-component gap split, fell back to deterministic 5s rule",
        ));
    }
    if !use_det {
        let (n_lex, n_tr, n_nvt) = records.iter().fold((0u64, 0u64, 0u64), |(l, t, v), r| {
            (
                l + u64::from(r.lex_div > 0.0),
                t + r.tr_ch,
                v + u64::from(r.nvt > 0.0),
            )
        });
        if n_lex > 0 || n_tr > 0 {
            bullet(&dim(&format!(
                "wordology axis: {} plain-text turns w/ lexical stats · {} tool_result turns · {} w/ cross-turn novelty → fused into labels",
                n_lex, n_tr, n_nvt
            )));
        }
    }

    let mut drv_n = 0u64;
    let mut drv_tok = 0u64;
    let mut drv_lats = Vec::new();
    let mut drv_in_total = 0u64;
    let mut bot_n = 0u64;
    let mut bot_tok = 0u64;
    let mut bot_lats = Vec::new();
    let mut bot_out = 0u64;
    let mut bot_cr = 0u64;
    let mut bot_cc = 0u64;

    // Driver gaps within session
    let mut drv_gaps: Vec<f64> = Vec::new();
    let mut by_sid: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in records.iter().enumerate() {
        let sid = r.sid.clone().unwrap_or_else(|| "_orphan".into());
        by_sid.entry(sid).or_default().push(i);
    }
    for (_, mut idxs) in by_sid.clone() {
        idxs.sort_by(|a, b| {
            records[*a]
                .ts
                .partial_cmp(&records[*b].ts)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut last_drv_ts: Option<f64> = None;
        for i in &idxs {
            if kinds[*i] == TurnKind::Driver {
                if let Some(prev) = last_drv_ts {
                    drv_gaps.push(records[*i].ts - prev);
                }
                last_drv_ts = Some(records[*i].ts);
            }
        }
    }

    // Bot loop length per driver turn
    let mut loop_lens: Vec<u32> = Vec::new();
    for (_, mut idxs) in by_sid {
        idxs.sort_by(|a, b| {
            records[*a]
                .ts
                .partial_cmp(&records[*b].ts)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut cur: u32 = 0;
        for i in &idxs {
            if kinds[*i] == TurnKind::Driver {
                if cur > 0 {
                    loop_lens.push(cur);
                }
                cur = 1;
            } else {
                cur += 1;
            }
        }
        if cur > 0 {
            loop_lens.push(cur);
        }
    }
    loop_lens.sort();

    for (i, r) in records.iter().enumerate() {
        let net_in = r.r#in.saturating_sub(r.cr).saturating_sub(r.cc);
        match kinds[i] {
            TurnKind::Driver => {
                drv_n += 1;
                drv_tok += net_in;
                drv_lats.push(r.lat);
                drv_in_total += r.r#in;
            }
            TurnKind::Bot => {
                bot_n += 1;
                bot_tok += net_in;
                bot_lats.push(r.lat);
                bot_out += r.out;
                bot_cr += r.cr;
                bot_cc += r.cc;
            }
        }
    }
    let _ = (drv_in_total,); // silence

    let total = drv_n + bot_n;
    let drv_pct = if total > 0 {
        drv_n as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    let bot_pct = if total > 0 {
        bot_n as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    let drv_avg_tok = if drv_n > 0 { drv_tok / drv_n } else { 0 };
    let bot_avg_tok = if bot_n > 0 { bot_tok / bot_n } else { 0 };
    let drv_avg_lat = if !drv_lats.is_empty() {
        drv_lats.iter().sum::<u64>() / drv_lats.len() as u64
    } else {
        0
    };
    let bot_avg_lat = if !bot_lats.is_empty() {
        bot_lats.iter().sum::<u64>() / bot_lats.len() as u64
    } else {
        0
    };

    println!();
    println!(
        "  {}   {:>4} turns  {}  drove   {:>9} tok  {}  {:>6} avg",
        bold("driver"),
        drv_n,
        grey("·"),
        fmt_with_commas(drv_tok),
        grey("·"),
        fmt_with_commas(drv_avg_tok)
    );
    if !drv_gaps.is_empty() {
        let mut sorted = drv_gaps.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let med = sorted[sorted.len() / 2];
        let p90_idx = (sorted.len() as f64 * 0.9) as usize;
        let p90 = sorted.get(p90_idx).copied().unwrap_or(med);
        println!(
            "           {}",
            dim(&format!(
                "gaps p50 {}   p90 {}   latency {}ms avg",
                fmt_dur(med),
                fmt_dur(p90),
                drv_avg_lat
            ))
        );
    } else {
        println!(
            "           {}",
            dim(&format!("single driver turn   latency {}ms", drv_avg_lat))
        );
    }

    let cache_total = bot_cr + bot_cc;
    let cache_str = if cache_total > 0 {
        format!(
            "  {}  cache reuse {}%",
            grey("·"),
            (bot_cr as f64 / cache_total as f64 * 100.0) as u64
        )
    } else {
        String::new()
    };
    println!(
        "  {}      {:>4} turns  {}  looped  {:>9} tok  {}  {:>6} avg{}",
        bold("bot"),
        bot_n,
        grey("·"),
        fmt_with_commas(bot_tok),
        grey("·"),
        fmt_with_commas(bot_avg_tok),
        cache_str
    );
    if !loop_lens.is_empty() {
        let p50 = loop_lens[loop_lens.len() / 2];
        let p90 = loop_lens
            .get((loop_lens.len() as f64 * 0.9) as usize)
            .copied()
            .unwrap_or(p50);
        let mx = *loop_lens.last().unwrap();
        println!(
            "           {}",
            dim(&format!(
                "loop length p50={}  p90={}  max={}   output {} tok   latency {}ms avg",
                p50,
                p90,
                mx,
                fmt_with_commas(bot_out),
                bot_avg_lat
            ))
        );
    } else {
        println!(
            "           {}",
            dim(&format!(
                "output {} tok   latency {}ms avg",
                fmt_with_commas(bot_out),
                bot_avg_lat
            ))
        );
    }

    section("ratio");
    let ratio_str = format!("{}/{}", drv_pct as u64, bot_pct as u64);
    let summary = dim(split_summary(drv_pct, bot_pct, drv_n, bot_n));
    println!("    {:>6}    {}", ratio_str, summary);

    if drv_n > 0 {
        let band = if drv_avg_tok > 8000 {
            dim("heavy: long prompts or paste-heavy")
        } else if drv_avg_tok < 200 {
            dim("light: short directive prompts")
        } else {
            dim("normal range")
        };
        println!(
            "    {}  {:>6}    {}",
            bold("driver tok"),
            fmt_with_commas(drv_avg_tok),
            band
        );
    }
    println!();
    Ok(())
}

fn fmt_with_commas(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}
