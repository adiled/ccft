//! METRICS strip — four hue-coded tiles, each its own bordered widget.
//!
//! Layout per tile (top to bottom):
//!
//!   title     — bold uppercase, in the tile's hue
//!   value     — the current aggregate, in the tile's hue
//!   sub-label — dim grey caption ("fine", "p99", "reuse")
//!   sparkline — bar histogram of the metric across the active range,
//!               in the tile's hue
//!
//! Tiles separated by 1-cell horizontal gutter (matching the rest of the
//! layout). Each tile gets its own corner-bracket panel chrome from the
//! shared `style::panel` renderer, so per-tile borders pick up the same
//! energized rail treatment as the larger panels.

use crate::brainrot::aggregate::{
    bot_score, compute_signal, driver_is_bootstrapping, driver_score, vibe_label, Aggregate,
    Baseline,
};
use crate::ledger_read::{percentile, Record};
use crate::tui::style;
use crate::tui::App;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let cells = Layout::default()
        .direction(Direction::Horizontal)
        .spacing(1)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(area);

    // Whole-range aggregates (the BIG number per tile).
    let bot = bot_score(&app.agg, &app.baseline);
    let drv = driver_score(&app.agg, &app.baseline);
    let driver_bootstrapping = driver_is_bootstrapping(&app.baseline);
    let (drv_value, drv_sub) = if driver_bootstrapping {
        ("—".to_string(), "type to start".to_string())
    } else {
        (drv.to_string(), vibe_label(drv).to_string())
    };
    let lats = app.agg.lats.clone();
    let p50 = percentile(&mut lats.clone(), 50.0) as u64;
    let lat_word = lat_tier_word(p50 as f64, &app.baseline);
    // Cache offload: of *all* input the model had to ingest, what fraction
    // came from cache (cheap path). Includes uncached `in` in the denominator
    // so the metric reflects how much of the total burden is being offloaded,
    // not just hit-rate within the cacheable portion.
    let total_input: u64 = app.agg.records.iter().map(|r| r.r#in + r.cr + r.cc).sum();
    let cache_read: u64 = app.agg.records.iter().map(|r| r.cr).sum();
    let _cache_pct = if total_input > 0 {
        cache_read as f64 / total_input as f64 * 100.0
    } else {
        0.0
    };

    // Sparkline series — bucket the records once across the active range
    // and compute each metric per bucket. Bucket count tracks tile inner
    // width so the sparkline naturally fits.
    let tile_inner_w = cells[0].width.saturating_sub(2) as usize;
    let n_buckets = tile_inner_w.max(8);
    let series = compute_series(app, n_buckets);

    tile(
        f,
        cells[0],
        "BOT",
        &bot.to_string(),
        &vibe_label(bot).to_string(),
        &series.bot,
        style::PINK,
    );
    tile(
        f,
        cells[1],
        "DRIVER",
        &drv_value,
        &drv_sub,
        if driver_bootstrapping {
            &[]
        } else {
            &series.driver
        },
        style::CYAN,
    );
    tile(
        f,
        cells[2],
        "LAT",
        &style::fmt_lat(p50),
        lat_word,
        &series.p99, // sparkline still shows p99-per-bucket for tail awareness
        style::GOLD,
    );
    let signal = compute_signal(&app.agg, &app.baseline);
    let bucket = crate::brainrot::aggregate::sigma_bucket(signal.z);
    tile(
        f,
        cells[3],
        "SIGNAL",
        &signal.phrase,
        &signal.value,
        &series.cache,
        style::signal_color(bucket),
    );
}

// ─── Latency tier word labels (dynamic against user's own baseline) ─────────

/// Map a current p50 latency to a word label by where it falls in the
/// user's own historical latency distribution. Each user's "quick" is
/// calibrated to *their* normal — no static thresholds.
///
/// Falls back to "—" when there's no baseline yet.
fn lat_tier_word(cur_ms: f64, baseline: &Baseline) -> &'static str {
    if baseline.lat_p80 <= 0.0 {
        return "—";
    }
    if cur_ms < baseline.lat_p20 {
        "quick"
    } else if cur_ms < baseline.lat_p40 {
        "fast"
    } else if cur_ms < baseline.lat_p60 {
        "mid"
    } else if cur_ms < baseline.lat_p80 {
        "slow"
    } else {
        "sloth"
    }
}

// ─── Tile rendering ──────────────────────────────────────────────────────────

fn tile(
    f: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    sub: &str,
    series: &[f32],
    hue: Color,
) {
    // Empty title — the tile renders its own bigger label below the chrome.
    let inner = style::panel(f, area, "");
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Adaptive vertical layout. We always render the title; everything
    // else folds in as height permits.
    //
    //   height >= 4   →  title / value / sub-label / sparkline
    //   height == 3   →  title / value /             sparkline
    //   height == 2   →  title / value
    //   height == 1   →  title only
    let h = inner.height;
    let title_y = inner.y;
    let value_y = inner.y + 1;
    let sub_y = inner.y + 2;
    let spark_y = inner.y + h - 1;

    // Title — bold + hue, centered.
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            label.to_string(),
            Style::default().fg(hue).add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Center),
        Rect {
            x: inner.x,
            y: title_y,
            width: inner.width,
            height: 1,
        },
    );

    // Value — same hue, no bold (per the global "values aren't bold" rule).
    if h >= 2 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                value.to_string(),
                Style::default().fg(hue),
            )))
            .alignment(Alignment::Center),
            Rect {
                x: inner.x,
                y: value_y,
                width: inner.width,
                height: 1,
            },
        );
    }

    // Sub-label — dim grey, centered. Only renders if it doesn't collide
    // with the sparkline row (need height >= 4).
    if h >= 4 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(sub.to_string(), style::dim())))
                .alignment(Alignment::Center),
            Rect {
                x: inner.x,
                y: sub_y,
                width: inner.width,
                height: 1,
            },
        );
    }

    // Sparkline — paint into the bottom row directly. Skips when height
    // is too small to fit alongside title + value.
    if h >= 3 {
        paint_sparkline(f.buffer_mut(), inner.x, spark_y, inner.width, series, hue);
    }
}

// ─── Sparkline rendering ─────────────────────────────────────────────────────

fn paint_sparkline(buf: &mut Buffer, x: u16, y: u16, width: u16, series: &[f32], hue: Color) {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max_w = width as usize;
    for (i, &v) in series.iter().take(max_w).enumerate() {
        let v = v.clamp(0.0, 1.0);
        if v < 0.05 {
            continue; // empty bucket — leave the cell as substrate
        }
        let idx = ((v * 7.99).floor() as usize).min(7);
        let cx = x + i as u16;
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(BARS[idx]);
            cell.set_style(Style::default().fg(hue));
        }
    }
}

// ─── Per-bucket series computation ───────────────────────────────────────────

struct Series {
    bot: Vec<f32>,
    driver: Vec<f32>,
    p99: Vec<f32>,
    cache: Vec<f32>,
}

fn compute_series(app: &App, n: usize) -> Series {
    // Bucket across the *active* span (first → last record), not the
    // calendar window. Otherwise a window like "today" with 30min of
    // activity in the last hour spreads its records into 1 of 30 buckets
    // and the chart looks blank. Falls back to the configured range when
    // there's no data.
    let (since, until) = match (app.agg.first_ts, app.agg.last_ts) {
        (Some(f), Some(l)) if l > f => (f, l),
        _ => (app.range.since, app.range.until),
    };
    let span = (until - since).max(1.0);
    let bucket_s = span / n as f64;

    let mut buckets: Vec<Vec<Record>> = (0..n).map(|_| Vec::new()).collect();
    for r in &app.agg.records {
        let idx = ((r.ts - since) / bucket_s).floor() as i64;
        if idx >= 0 && (idx as usize) < n {
            buckets[idx as usize].push(r.clone());
        } else if idx as usize >= n {
            // Last record exactly at `until` lands at idx=n; clamp.
            buckets[n - 1].push(r.clone());
        }
    }

    let mut bot = vec![0.0_f32; n];
    let mut driver = vec![0.0_f32; n];
    let mut p99 = vec![0.0_f32; n];
    let mut cache = vec![0.0_f32; n];

    for (i, bucket) in buckets.into_iter().enumerate() {
        if bucket.is_empty() {
            continue;
        }
        let mut lats: Vec<u64> = bucket.iter().map(|r| r.lat).collect();
        let p99_v = percentile(&mut lats, 99.0) as f32;
        let cr_total: u64 = bucket.iter().map(|r| r.cr).sum();
        let cc_total: u64 = bucket.iter().map(|r| r.cc).sum();
        let in_total: u64 = bucket.iter().map(|r| r.r#in).sum();
        let total_in = in_total + cr_total + cc_total;

        let agg = Aggregate::ingest(bucket);
        bot[i] = bot_score(&agg, &app.baseline) as f32 / 100.0;
        driver[i] = driver_score(&agg, &app.baseline) as f32 / 100.0;
        p99[i] = p99_v;
        cache[i] = if total_in > 0 {
            cr_total as f32 / total_in as f32
        } else {
            0.0
        };
    }

    // Normalize p99 against the max in its own series — p99 latency has no
    // fixed ceiling like bot/driver/cache do, so the bars are relative.
    let p99_max = p99.iter().fold(0.0_f32, |a, b| a.max(*b)).max(1e-6);
    for v in p99.iter_mut() {
        *v /= p99_max;
    }

    Series {
        bot,
        driver,
        p99,
        cache,
    }
}
