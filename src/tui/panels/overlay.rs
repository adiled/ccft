//! Overlay rendering — modal panes for sessions / perf / help.
//!
//! Each overlay is a self-contained read-only view computed from the
//! current `app.agg.records` (already range-filtered). Esc returns to
//! the main view.

use crate::tui::style;
use crate::tui::{App, Overlay};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let popup = centered(area, 80, 70);
    // Clear strips any text under the popup. Then a solid BG fill so the
    // overlay reads as its own opaque surface — without it the substrate
    // color of cells under the overlay leaks through and the popup looks
    // see-through.
    f.render_widget(Clear, popup);
    f.render_widget(
        Block::default().style(Style::default().bg(style::BG)),
        popup,
    );

    let title = match app.overlay {
        Overlay::Sessions => "sessions",
        Overlay::Perf => "perf",
        Overlay::Help => "help",
        Overlay::None => return,
    };

    let inner = style::panel(f, popup, title);

    let lines: Vec<Line> = match app.overlay {
        Overlay::Help => help_lines(),
        Overlay::Perf => perf_lines(app),
        Overlay::Sessions => sessions_lines(app),
        Overlay::None => return,
    };

    f.render_widget(Paragraph::new(lines), inner);
}

// ─── Perf ─────────────────────────────────────────────────────────────────────

fn perf_lines(app: &App) -> Vec<Line<'static>> {
    let records = &app.agg.records;
    let label = app.range.label.clone();

    let mut walls: Vec<u64> = Vec::new();
    let mut upstreams: Vec<u64> = Vec::new();
    let mut pres: Vec<u64> = Vec::new();
    let mut ccfts: Vec<u64> = Vec::new();
    let mut walls_with_ccft: Vec<u64> = Vec::new();
    let mut n_total = 0u64;

    for r in records {
        n_total += 1;
        let wall_ms = ((r.te - r.ts) * 1000.0) as i64;
        if wall_ms <= 0 {
            continue;
        }
        let wall_ms = wall_ms as u64;
        walls.push(wall_ms);
        upstreams.push(r.lat);
        pres.push(wall_ms.saturating_sub(r.lat));
        if let Some(c_us) = r.c_us {
            if c_us > 0 {
                ccfts.push(c_us);
                walls_with_ccft.push(wall_ms);
            }
        }
    }

    let n_with_ccft = ccfts.len() as u64;

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            format!("ccft perf · {}", label),
            style::title(),
        )),
        Line::from(""),
    ];

    if n_total == 0 {
        lines.push(Line::from(Span::styled(
            "(no records in range)",
            style::dim(),
        )));
        return lines;
    }

    lines.push(perf_row("wall", &walls, fmt_ms_u, false));
    lines.push(perf_row("upstream", &upstreams, fmt_ms_u, true));
    lines.push(perf_row("pre", &pres, fmt_ms_u, true));
    if !ccfts.is_empty() {
        lines.push(perf_row("ccft", &ccfts, fmt_us_u, false));
    } else {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:10}  ", "ccft"), style::label()),
            Span::styled(
                "no records with ccft timing yet — run more traffic",
                style::dim(),
            ),
        ]));
    }

    lines.push(Line::from(""));
    let coverage_pct = if n_total > 0 {
        n_with_ccft as f64 / n_total as f64 * 100.0
    } else {
        0.0
    };
    lines.push(Line::from(vec![
        Span::styled("  records   ", style::label()),
        Span::styled(n_total.to_string(), style::value()),
        Span::styled("  ·  ", style::dim()),
        Span::styled(format!("{} with ccft timing", n_with_ccft), style::dim()),
        Span::styled("  ·  ", style::dim()),
        Span::styled("wall = upstream + pre", style::dim()),
    ]));

    if !ccfts.is_empty() {
        let ccft_p50 = percentile(&mut ccfts.clone(), 50.0);
        let wall_p50 = percentile(&mut walls_with_ccft.clone(), 50.0);
        let (verdict_color, msg) = perf_verdict(ccft_p50, wall_p50, coverage_pct);
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("  verdict   ", style::label()),
            Span::styled(msg, Style::default().fg(verdict_color)),
        ]));
    }

    lines
}

fn perf_row(name: &str, values: &[u64], fmt: fn(f64) -> String, dim: bool) -> Line<'static> {
    let mut owned: Vec<u64> = values.to_vec();
    let p50 = percentile(&mut owned, 50.0);
    let p95 = percentile(&mut owned, 95.0);
    let p99 = percentile(&mut owned, 99.0);
    let val_style = if dim { style::dim() } else { style::value() };
    Line::from(vec![
        Span::styled(format!("  {:10}  ", name), style::label()),
        Span::styled("p50 ", style::dim()),
        Span::styled(format!("{:>9}", fmt(p50)), val_style),
        Span::styled("  p95 ", style::dim()),
        Span::styled(format!("{:>9}", fmt(p95)), val_style),
        Span::styled("  p99 ", style::dim()),
        Span::styled(format!("{:>9}", fmt(p99)), val_style),
    ])
}

fn perf_verdict(
    ccft_p50_us: f64,
    wall_p50_ms: f64,
    coverage_pct: f64,
) -> (ratatui::style::Color, String) {
    let ccft_ms = ccft_p50_us / 1000.0;
    let rel = if wall_p50_ms > 0.0 {
        ccft_ms / wall_p50_ms * 100.0
    } else {
        0.0
    };
    let sample_warn = if coverage_pct < 5.0 {
        format!(" (small sample — {:.0}% of records)", coverage_pct)
    } else {
        String::new()
    };
    if ccft_ms < 5.0 && rel < 1.0 {
        (
            style::LIME,
            format!(
                "ccft contributes ~{:.2}% of wall time. not the bottleneck — slowness is upstream.{}",
                rel, sample_warn
            ),
        )
    } else if ccft_ms < 30.0 && rel < 3.0 {
        (
            style::LIME,
            format!(
                "ccft adds ~{:.1}ms median ({:.1}%). small, well within network noise.{}",
                ccft_ms, rel, sample_warn
            ),
        )
    } else if ccft_ms < 100.0 && rel < 10.0 {
        (
            style::GOLD,
            format!(
                "ccft adds ~{:.0}ms median ({:.0}%). measurable but probably acceptable.{}",
                ccft_ms, rel, sample_warn
            ),
        )
    } else {
        (
            style::PINK,
            format!(
                "⚠ ccft adds ~{:.0}ms median ({:.0}% of wall). worth investigating.{}",
                ccft_ms, rel, sample_warn
            ),
        )
    }
}

// ─── Sessions ─────────────────────────────────────────────────────────────────

fn sessions_lines(app: &App) -> Vec<Line<'static>> {
    struct SessAgg {
        n: u64,
        tot: u64,
        lat_sum: u64,
        first: f64,
        last: f64,
        models: HashMap<String, u64>,
    }

    let mut sessions: HashMap<String, SessAgg> = HashMap::new();
    for r in &app.agg.records {
        let sid = r.sid.clone().unwrap_or_else(|| "(no-sid)".into());
        let s = sessions.entry(sid).or_insert(SessAgg {
            n: 0,
            tot: 0,
            lat_sum: 0,
            first: r.ts,
            last: r.ts,
            models: HashMap::new(),
        });
        s.n += 1;
        s.tot += r.tot;
        s.lat_sum += r.lat;
        s.first = s.first.min(r.ts);
        s.last = s.last.max(r.ts);
        let m = r.model.clone().unwrap_or_else(|| "?".into());
        *s.models.entry(m).or_insert(0) += 1;
    }

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            format!("sessions · {} ({})", app.range.label, sessions.len()),
            style::title(),
        )),
        Line::from(""),
    ];

    if sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "no sessions in range",
            style::dim(),
        )));
        return lines;
    }

    lines.push(Line::from(vec![
        Span::styled("  sid       ", style::dim()),
        Span::styled("  reqs", style::dim()),
        Span::styled("    tokens", style::dim()),
        Span::styled("   avg lat", style::dim()),
        Span::styled("     span", style::dim()),
        Span::styled("  model", style::dim()),
    ]));

    let mut entries: Vec<(&String, &SessAgg)> = sessions.iter().collect();
    entries.sort_by(|a, b| b.1.n.cmp(&a.1.n));

    for (sid, s) in entries.into_iter().take(20) {
        let sid_short = if sid == "(no-sid)" {
            sid.clone()
        } else {
            sid.chars().take(8).collect::<String>()
        };
        let avg_lat = if s.n > 0 { s.lat_sum / s.n } else { 0 };
        let span = fmt_dur(s.last - s.first);
        let top_model = s
            .models
            .iter()
            .max_by_key(|(_, c)| **c)
            .map(|(m, _)| short_model(m))
            .unwrap_or_else(|| "?".into());

        lines.push(Line::from(vec![
            Span::styled(format!("  {:10}", sid_short), style::label()),
            Span::styled(format!("  {:>4}", s.n), style::value()),
            Span::styled(format!("  {:>8}", short_n(s.tot)), style::value()),
            Span::styled(
                format!("   {:>6}", style::fmt_lat(avg_lat)),
                Style::default().fg(style::heat_color(avg_lat as f64)),
            ),
            Span::styled(format!("  {:>7}", span), style::label()),
            Span::styled(format!("  {}", top_model), style::dim()),
        ]));
    }

    lines
}

// ─── Help ─────────────────────────────────────────────────────────────────────

fn help_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled("Range dial", style::title())),
        Line::from(vec![
            Span::styled("  ←/→  ", style::key_hint()),
            Span::styled("step preset", style::label()),
        ]),
        Line::from(vec![
            Span::styled("  t y h w W a  ", style::key_hint()),
            Span::styled("today / yday / 24h / 7d / this-week / all", style::label()),
        ]),
        Line::from(""),
        Line::from(Span::styled("Drill overlays", style::title())),
        Line::from(vec![
            Span::styled("  s  ", style::key_hint()),
            Span::styled("sessions list", style::label()),
        ]),
        Line::from(vec![
            Span::styled("  p  ", style::key_hint()),
            Span::styled("perf decomposition", style::label()),
        ]),
        Line::from(""),
        Line::from(Span::styled("Other", style::title())),
        Line::from(vec![
            Span::styled("  r  ", style::key_hint()),
            Span::styled("force refresh", style::label()),
        ]),
        Line::from(vec![
            Span::styled("  Esc / q  ", style::key_hint()),
            Span::styled("close overlay / quit TUI", style::label()),
        ]),
    ]
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn percentile(values: &mut [u64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable();
    let k = (values.len() - 1) as f64 * p / 100.0;
    let f = k.floor() as usize;
    let c = (f + 1).min(values.len() - 1);
    values[f] as f64 + (values[c] as f64 - values[f] as f64) * (k - f as f64)
}

fn fmt_ms_u(ms: f64) -> String {
    if ms < 1.0 {
        format!("{}us", (ms * 1000.0) as u64)
    } else if ms < 1000.0 {
        format!("{}ms", ms as u64)
    } else {
        format!("{:.1}s", ms / 1000.0)
    }
}

fn fmt_us_u(us: f64) -> String {
    if us < 1000.0 {
        format!("{}us", us as u64)
    } else if us < 1_000_000.0 {
        format!("{:.1}ms", us / 1000.0)
    } else {
        format!("{:.1}s", us / 1_000_000.0)
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

fn short_n(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn short_model(m: &str) -> String {
    let stripped = m.strip_prefix("claude-").unwrap_or(m);
    let parts: Vec<&str> = stripped.split('-').collect();
    if parts.len() >= 2 {
        format!("{}-{}", parts[0], parts[1])
    } else {
        stripped.to_string()
    }
}

fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let w = area.width * pct_w / 100;
    let h = area.height * pct_h / 100;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}
