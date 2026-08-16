//! Diagnosis panel — vibe + split + peaks + models. Center-column display.
//! Driver/bot dynamics alongside vibe label and operational notes.

use crate::brainrot::aggregate::{
    classify_turns_prob, diagnosis, score_breakdown, short_model, split_summary, vibe_label,
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
    let inner = style::panel(f, area, "diagnosis");

    let bd = score_breakdown(&app.agg, &app.baseline);
    let bot = bd.b_shrunk as u32;
    let drv = bd.d_shrunk as u32;
    let mut lines: Vec<Line> = Vec::new();

    // ── vibe ────────────────────────────────────────────────────────────
    lines.push(Line::from(vec![
        section_label("vibe    ", style::PINK),
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

    // ── note ────────────────────────────────────────────────────────────
    if let Some(d) = diagnosis(&bd) {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            section_label("note   ", style::GOLD),
            note(d.to_string()),
        ]));
    }

    // ── split summary (driver vs bot turns) ─────────────────────────────
    if let Some(s) = compute_split(app) {
        lines.push(Line::from(""));
        let summary = split_summary(s.drv_pct, s.bot_pct, s.drv_n, s.bot_n);
        // "split   23/77   5 drv  17 bot    ·  bot-heavy …"
        // Color the percentages by their owning side: driver=CYAN, bot=PINK.
        lines.push(Line::from(vec![
            section_label("split  ", style::CYAN),
            Span::styled(
                format!("{}", s.drv_pct as u64),
                Style::default()
                    .fg(style::CYAN)
                    .add_modifier(Modifier::BOLD),
            ),
            connector("/".to_string()),
            Span::styled(
                format!("{}", s.bot_pct as u64),
                Style::default()
                    .fg(style::PINK)
                    .add_modifier(Modifier::BOLD),
            ),
            connector(format!("   {} drv  ", s.drv_n)),
            connector(format!("{} bot", s.bot_n)),
            sep(),
            note(summary.to_string()),
        ]));

        // Loop length + driver gap median + cache offload, on a sub-line
        // when present.
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
            section_label("peaks  ", style::LIME),
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
    if !app.agg.models.is_empty() {
        let total: u64 = app.agg.models.values().sum();
        let mut sorted: Vec<(&String, &u64)> = app.agg.models.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        lines.push(Line::from(""));
        let mut spans: Vec<Span> = vec![section_label("models ", style::VIOLET)];
        for (idx, (m, c)) in sorted.iter().take(3).enumerate() {
            if idx > 0 {
                spans.push(sep());
            }
            let pct = if total > 0 {
                **c as f64 / total as f64 * 100.0
            } else {
                0.0
            };
            spans.push(Span::styled(
                short_model(m),
                Style::default().fg(style::VIOLET),
            ));
            spans.push(connector(" ".to_string()));
            spans.push(num(format!("{:.0}%", pct)));
        }
        lines.push(Line::from(spans));
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
