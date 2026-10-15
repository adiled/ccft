//! Diagnosis panel — vibe + split + peaks + models. Center-column display.
//! Driver/bot dynamics alongside vibe label and operational notes.

use crate::brainrot::aggregate::{
    classify_turns_prob, diagnosis, score_breakdown, split_summary, vibe_label,
    TurnKind,
};
use crate::tui::style;
use crate::tui::App;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use std::collections::HashMap;

/// Bold + hue for section labels.
fn section_label(text: &str, hue: ratatui::style::Color) -> Span<'_> {
    Span::styled(
        text.to_string(),
        Style::default().fg(hue).add_modifier(Modifier::BOLD),
    )
}

/// White + bold for numerical "headline" values (counts, percentages).
fn num(text: String) -> Span<'static> {
    Span::styled(
        text,
        Style::default()
            .fg(style::WHITE)
            .add_modifier(Modifier::BOLD),
    )
}

/// Cyan for general data values that aren't headline numbers (durations,
/// less prominent metrics).
fn data(text: String) -> Span<'static> {
    Span::styled(text, Style::default().fg(style::CYAN))
}

/// Subtle italic for advisory / commentary text.
fn note(text: String) -> Span<'static> {
    Span::styled(
        text,
        Style::default()
            .fg(style::SUBTLE)
            .add_modifier(Modifier::ITALIC),
    )
}

/// Dim grey separator " · ".
fn sep() -> Span<'static> {
    Span::styled("  ·  ".to_string(), style::dim())
}

/// Dim connecting word ("over", "and", etc.).
fn connector(text: String) -> Span<'static> {
    Span::styled(text, style::dim())
}

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let inner = style::panel(f, area, "deep dive");

    let bd = score_breakdown(&app.agg, &app.baseline);
    let bot = bd.b_shrunk as u32;
    let drv = bd.d_shrunk as u32;
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        section_label("vibe    ", style::SUBTLE),
        Span::styled(
            format!("bot {}", vibe_label(bot)),
            Style::default()
                .fg(style::score_color(bot))
                .add_modifier(Modifier::BOLD),
        ),
        sep(),
        Span::styled(
            format!("driver {}", vibe_label(drv)),
            Style::default()
                .fg(style::score_color(drv))
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    if let Some(d) = diagnosis(&bd) {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            section_label("note   ", style::SUBTLE),
            note(d.to_string()),
        ]));
    }
    if let Some(s) = compute_split(app) {
        lines.push(Line::from(""));
        let summary = split_summary(s.drv_pct, s.bot_pct, s.drv_n, s.bot_n);
        lines.push(Line::from(vec![
            section_label("split  ", style::SUBTLE),
            note(summary.to_string()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("       ", style::dim()),
            connector("drv ".to_string()),
            Span::styled(
                format!("{:.0}%", s.drv_pct),
                Style::default()
                    .fg(style::CYAN)
                    .add_modifier(Modifier::BOLD),
            ),
            connector(format!(" ({})", s.drv_n)),
            sep(),
            connector("bot ".to_string()),
            Span::styled(
                format!("{:.0}%", s.bot_pct),
                Style::default()
                    .fg(style::PINK)
                    .add_modifier(Modifier::BOLD),
            ),
            connector(format!(" ({})", s.bot_n)),
        ]));
        let mut sub_bits: Vec<Span> = Vec::new();
        if !s.loop_lens.is_empty() {
            let p50 = s.loop_lens[s.loop_lens.len() / 2];
            let p90 = s
                .loop_lens
                .get((s.loop_lens.len() as f64 * 0.9) as usize)
                .copied()
                .unwrap_or(p50);
            sub_bits.push(connector("loop ".to_string()));
            sub_bits.push(num(format!("{}", p50)));
            sub_bits.push(connector("/".to_string()));
            sub_bits.push(num(format!("{}", p90)));
            sub_bits.push(connector(" (p50/p90)".to_string()));
        }
        if !s.drv_gaps.is_empty() {
            let mut g = s.drv_gaps.clone();
            g.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let med = g[g.len() / 2];
            if !sub_bits.is_empty() {
                sub_bits.push(sep());
            }
            sub_bits.push(connector("drv gap ".to_string()));
            sub_bits.push(data(fmt_dur(med)));
        }
        if s.cache_total > 0 {
            let pct = s.cache_reuse_n as f64 / s.cache_total as f64 * 100.0;
            if !sub_bits.is_empty() {
                sub_bits.push(sep());
            }
            sub_bits.push(connector("cache offload ".to_string()));
            sub_bits.push(Span::styled(
                format!("{:.0}%", pct),
                Style::default()
                    .fg(style::LIME)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if !sub_bits.is_empty() {
            let mut prefix = vec![Span::styled("       ", style::dim())];
            prefix.extend(sub_bits);
            lines.push(Line::from(prefix));
        }
    }
    // ── peaks ───────────────────────────────────────────────────────────
    if !app.agg.by_hour.is_empty() {
        let peak = app
            .agg
            .by_hour
            .iter()
            .max_by_key(|(_, b)| b.n)
            .map(|(h, b)| (*h, b.n))
            .unwrap_or((0, 0));
        let slow = app
            .agg
            .by_hour
            .iter()
            .max_by(|x, y| {
                let lx = x.1.lat_sum as f64 / x.1.n.max(1) as f64;
                let ly = y.1.lat_sum as f64 / y.1.n.max(1) as f64;
                lx.partial_cmp(&ly).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(h, b)| (*h, b.lat_sum as f64 / b.n.max(1) as f64))
            .unwrap_or((0, 0.0));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            section_label("peaks  ", style::SUBTLE),
            connector("busy ".to_string()),
            num(format!("{:02}:00", peak.0)),
            connector(" (".to_string()),
            data(format!("{} reqs", peak.1)),
            connector(")".to_string()),
            sep(),
            connector("slow ".to_string()),
            num(format!("{:02}:00", slow.0)),
            connector(" (".to_string()),
            Span::styled(
                style::fmt_lat(slow.1.round() as u64),
                Style::default()
                    .fg(style::heat_color(slow.1))
                    .add_modifier(Modifier::BOLD),
            ),
            connector(")".to_string()),
        ]));
    }

    // ── models ──────────────────────────────────────────────────────────
    if !app.agg.records.is_empty() {
        let mut toks: HashMap<&String, u64> = HashMap::new();
        let mut lat: HashMap<&String, (u64, u64)> = HashMap::new();
        for r in &app.agg.records {
            if let Some(m) = &r.model {
                *toks.entry(m).or_insert(0) += r.tot;
                let e = lat.entry(m).or_insert((0, 0));
                e.0 += r.lat;
                e.1 += 1;
            }
        }
        let total: u64 = toks.values().sum();
        let mut sorted: Vec<(&String, u64)> = toks.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        lines.push(Line::from(""));
        let mut spans: Vec<Span> = vec![section_label("models ", style::SUBTLE)];
        let solo = sorted.len() == 1;
        for (idx, (m, t)) in sorted.iter().take(3).enumerate() {
            if idx > 0 {
                spans.push(sep());
            }
            let pct = if total > 0 {
                *t as f64 / total as f64 * 100.0
            } else {
                0.0
            };
            spans.push(Span::styled(
                (*m).clone(),
                Style::default().fg(style::VIOLET),
            ));
            if !solo {
                spans.push(connector(" ".to_string()));
                spans.push(num(format!("{:.0}%", pct)));
            }
            let avg = lat.get(m).map(|(a, b)| *a as f64 / (*b).max(1) as f64).unwrap_or(0.0);
            spans.push(connector(" ".to_string()));
            spans.push(Span::styled(
                style::fmt_lat(avg.round() as u64),
                Style::default().fg(style::heat_color(avg)),
            ));
        }
        lines.push(Line::from(spans));
    }
    {
        lines.push(Line::from(""));
        let th: u64 = app.agg.records.iter().map(|r| r.th_ch).sum();
        let tr: u64 = app.agg.records.iter().map(|r| r.tr_ch).sum();
        let nvt = if app.agg.records.is_empty() {
            0.0
        } else {
            app.agg.records.iter().map(|r| r.nvt).sum::<f64>() / app.agg.records.len() as f64 * 100.0
        };
        lines.push(Line::from(vec![
            section_label("vol    ", style::SUBTLE),
            connector("in ".to_string()),
            num(compact(app.agg.r#in)),
            sep(),
            connector("out ".to_string()),
            num(compact(app.agg.out)),
            sep(),
            connector("sess ".to_string()),
            num(format!("{}", app.agg.sessions.len())),
        ]));
        lines.push(Line::from(vec![
            section_label("tex    ", style::SUBTLE),
            connector("think ".to_string()),
            data(format!("{}", th)),
            sep(),
            connector("tool ".to_string()),
            data(format!("{}", tr)),
            sep(),
            connector("novelty ".to_string()),
            num(format!("{:.0}%", nvt)),
        ]));
        let cov = crate::ledger_read::compute_coverage(
            &crate::ledger_read::load_state_events(),
            app.range.since,
            app.range.until,
        );
        let apct = if cov.total_s > 0.0 { cov.active_s / cov.total_s * 100.0 } else { 100.0 };
        lines.push(Line::from(vec![
            section_label("cov    ", style::SUBTLE),
            connector("active ".to_string()),
            num(format!("{:.0}%", apct)),
            sep(),
            connector("gaps ".to_string()),
            num(format!("{}", cov.off_intervals.len())),
        ]));
    }

    // Cluster into the upper-left of the panel. Text gets the full panel
    // width so long lines (split summary, peaks with heat-colored lat,
    // models row) aren't silently truncated. The lower-right negative
    // space comes from height — we only use as many rows as there are
    // lines, leaving everything below empty.
    let line_count = lines.len() as u16;
    let text_h = (line_count + 1).min(inner.height);
    let text_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: text_h,
    };

    f.render_widget(Paragraph::new(lines), text_area);
}

// ─── Split summary computation ───────────────────────────────────────────────

struct SplitSummary {
    drv_n: u64,
    bot_n: u64,
    drv_pct: f64,
    bot_pct: f64,
    drv_gaps: Vec<f64>,
    loop_lens: Vec<u64>,
    cache_reuse_n: u64,
    cache_total: u64,
}

/// Returns None if there are no records (skip the split row entirely).
fn compute_split(app: &App) -> Option<SplitSummary> {
    let records = &app.agg.records;
    if records.is_empty() {
        return None;
    }
    let kinds = classify_turns_prob(records);

    let mut drv_n = 0u64;
    let mut bot_n = 0u64;
    let mut bot_cr = 0u64;
    let mut bot_cc = 0u64;
    let mut bot_in = 0u64;

    // Per-session walk for gap & loop length stats.
    let mut by_sid: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in records.iter().enumerate() {
        let sid = r.sid.clone().unwrap_or_else(|| "_orphan".into());
        by_sid.entry(sid).or_default().push(i);
    }

    let mut drv_gaps: Vec<f64> = Vec::new();
    let mut loop_lens: Vec<u64> = Vec::new();
    for (_sid, mut idxs) in by_sid {
        idxs.sort_by(|a, b| {
            records[*a]
                .ts
                .partial_cmp(&records[*b].ts)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut last_drv_ts: Option<f64> = None;
        let mut cur: u64 = 0;
        for i in &idxs {
            if kinds[*i] == TurnKind::Driver {
                if let Some(prev) = last_drv_ts {
                    drv_gaps.push(records[*i].ts - prev);
                }
                last_drv_ts = Some(records[*i].ts);
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

    for (i, r) in records.iter().enumerate() {
        match kinds[i] {
            TurnKind::Driver => drv_n += 1,
            TurnKind::Bot => {
                bot_n += 1;
                bot_cr += r.cr;
                bot_cc += r.cc;
                bot_in += r.r#in;
            }
        }
    }

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

    loop_lens.sort();

    Some(SplitSummary {
        drv_n,
        bot_n,
        drv_pct,
        bot_pct,
        drv_gaps,
        loop_lens,
        cache_reuse_n: bot_cr,
        cache_total: bot_in + bot_cr + bot_cc,
    })
}

fn compact(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn fmt_dur(secs: f64) -> String {
    let s = secs.abs();
    if s < 60.0 {
        format!("{}s", s as u64)
    } else if s < 3600.0 {
        let m = (s / 60.0) as u64;
        let sec = (s as u64) % 60;
        format!("{}m{:02}s", m, sec)
    } else if s < 86400.0 {
        let h = (s / 3600.0) as u64;
        let m = ((s as u64) % 3600) / 60;
        format!("{}h{:02}m", h, m)
    } else {
        let d = (s / 86400.0) as u64;
        let h = ((s as u64) % 86400) / 3600;
        format!("{}d{}h", d, h)
    }
}
