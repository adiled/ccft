//! `ccft brainrot debug-scores [range]` — dumps every component of bot
//! and driver scores so we can see *why* a window scored what it did.
//! Internal validation tool, not user-facing.

use crate::brainrot::aggregate::*;
use crate::ledger_read::{iter_records, parse_range};

pub fn run(spec: &str) -> Result<(), Box<dyn std::error::Error>> {
    let spec = if spec.trim().is_empty() {
        "today"
    } else {
        spec
    };
    let range = parse_range(spec).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let a = Aggregate::ingest(iter_records(Some(range.since), Some(range.until)));
    let baseline_records: Vec<_> = iter_records(None, None).collect();
    let baseline = Baseline::from_records(&baseline_records);

    println!("== brainrot debug-scores · {} ==", range.label);
    println!();
    println!(
        "baseline n={}  records_with_u_ch={}",
        baseline.n_records, baseline.n_records_with_u_ch
    );
    println!("  driver:");
    println!(
        "    user_chars_per_min   mean={:.2}  std={:.2}  (winsorized 5/95)",
        baseline.user_chars_per_min_mean, baseline.user_chars_per_min_std
    );
    println!(
        "                         med={:.2}   mad={:.2}  (robust, for ref)",
        baseline.user_chars_per_min_med, baseline.user_chars_per_min_mad
    );
    println!("  bot:");
    println!(
        "    out                  med={:.0}  mad={:.0}",
        baseline.out_med, baseline.out_mad
    );
    println!(
        "    ms_per_token         med={:.2}  mad={:.2}",
        baseline.ms_per_token_med, baseline.ms_per_token_mad
    );
    println!("    cache_miss_rate      {:.3}", baseline.cache_miss_rate);
    println!(
        "    session_out_cv       med={:.2}  mad={:.2}",
        baseline.session_out_cv_med, baseline.session_out_cv_mad
    );
    println!("  latency tiers (ms):");
    println!(
        "    p20={:.0}  p40={:.0}  p60={:.0}  p80={:.0}",
        baseline.lat_p20, baseline.lat_p40, baseline.lat_p60, baseline.lat_p80
    );
    println!();

    if a.n == 0 {
        println!("(no records in window)");
        return Ok(());
    }

    let bd = score_breakdown(&a, &baseline);

    let span_min = (a.last_ts.unwrap_or(0.0) - a.first_ts.unwrap_or(0.0)) / 60.0;
    println!(
        "window  n={}  sessions={}  span={:.1}min",
        bd.n,
        a.sessions.len(),
        span_min
    );
    let avg_in = a.r#in as f64 / a.n.max(1) as f64;
    let avg_out = a.out as f64 / a.n.max(1) as f64;
    println!(
        "        avg_in={:.0}  avg_out={:.0}  reqs/min={:.2}",
        avg_in,
        avg_out,
        a.n as f64 / span_min.max(1.0)
    );
    println!(
        "        confidence={:.3}  (shrinkage = {:.0}%)",
        bd.confidence,
        (1.0 - bd.confidence) * 100.0
    );
    println!();

    println!("DRIVER (kinetics: user-typed chars/min)");
    println!("  records_with_u_ch    {} / {}", bd.d_with_u_ch, bd.n);
    println!("  total u_ch in window {}", bd.d_total_u_ch);
    println!("  current  chars/min   {:.2}", bd.d_chars_per_min);
    println!(
        "  baseline chars/min   {:.2}  (std={:.2})",
        bd.d_baseline_cpm, bd.d_baseline_mad
    );
    println!("  z                    {:+.2}", bd.d_z);
    println!("  raw                  {:>6.2}", bd.d_raw);
    println!(
        "  shrunk               {:>6.2}  → {} / 100",
        bd.d_shrunk,
        bd.d_shrunk.round().clamp(0.0, 100.0) as u32
    );
    println!();

    println!("BOT components (raw → weighted)");
    println!(
        "  brevity       {:>6.1}   × 0.25 = {:>6.2}",
        bd.b_brevity,
        bd.b_brevity * 0.25
    );
    println!(
        "  stalling      {:>6.1}   × 0.20 = {:>6.2}",
        bd.b_stalling,
        bd.b_stalling * 0.20
    );
    println!(
        "  wandering     {:>6.1}   × 0.20 = {:>6.2}",
        bd.b_wandering,
        bd.b_wandering * 0.20
    );
    println!(
        "  cache_drag    {:>6.1}   × 0.10 = {:>6.2}",
        bd.b_cache_drag,
        bd.b_cache_drag * 0.10
    );
    println!(
        "  thinking_rot  {:>6.1}   × 0.25 = {:>6.2}  (th_ch/out × (1-lex_div))",
        bd.b_thinking_rot,
        bd.b_thinking_rot * 0.25
    );
    println!("   ─────────────────────────────────");
    println!("  raw composite       = {:>6.2}", bd.b_raw);
    println!(
        "  shrunk              = {:>6.2}   → {} / 100",
        bd.b_shrunk,
        bd.b_shrunk.round().clamp(0.0, 100.0) as u32
    );

    Ok(())
}
