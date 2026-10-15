//! LEDGER — newest-first table. Header above, tight rows below. The
//! brief: header 8% / rows 92%, no roomy spacing — claustrophobic
//! density, surveilling.

use crate::tui::style;
use crate::tui::App;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::Span;
use ratatui::widgets::{Cell, Row, Table};
use ratatui::Frame;

/// How many records to fetch from the file for the ledger table.
const LEDGER_TABLE_MAX_ROWS: usize = 320;

pub fn render(f: &mut Frame, area: Rect, _app: &App) {
    let inner = style::panel_with(f, area, "ledger", 1);
    let local = crate::tui::style::local_offset();

    let header = Row::new(vec![
        Cell::from(Span::styled("TIME", style::dim())),
        Cell::from(Span::styled("drv·in", style::dim())),
        Cell::from(Span::styled("bot·out", style::dim())),
        Cell::from(Span::styled("lat", style::dim())),
    ]);

    // Tight: only header row reserved (1 row), all remaining rows are data.
    // No bottom margin.
    let max_rows = inner.height.saturating_sub(1) as usize;

    // Fetch directly from file — skips any app.agg.records and avoids
    // cloning the whole table.  load_top_records reads only the tail end
    // of each ledger file so even multi-million-line ledgers never block.
    let mut recs = crate::ledger_read::load_top_records(max_rows.max(LEDGER_TABLE_MAX_ROWS));
    recs.sort_by(|a, b| b.ts.partial_cmp(&a.ts).unwrap_or(std::cmp::Ordering::Equal));

    let mut rows: Vec<Row> = Vec::new();
    let mut prev_day: Option<(i32, u8, u8)> = None;
    for r in recs.iter() {
        if rows.len() >= max_rows {
            break;
        }
        let dt0 = time::OffsetDateTime::from_unix_timestamp(r.ts as i64)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
            .to_offset(local);
        let day = (dt0.year(), dt0.month() as u8, dt0.day());
        if let Some(p) = prev_day {
            if p != day && rows.len() + 1 < max_rows {
                let sep = "─".repeat(8);
                rows.push(Row::new(vec![
                    Cell::from(Span::styled(sep, style::dim())),
                    Cell::from(Span::styled("─".repeat(7), style::dim())),
                    Cell::from(Span::styled("─".repeat(7), style::dim())),
                    Cell::from(Span::styled("─".repeat(7), style::dim())),
                ]));
            }
        }
        prev_day = Some(day);
            let dt = time::OffsetDateTime::from_unix_timestamp(r.ts as i64)
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
                .to_offset(local);
            let when = format!("{:02}:{:02}:{:02}", dt.hour(), dt.minute(), dt.second());
            // Lat at 50% opacity — heat-colored but visually receded so
            // the time + token columns dominate.
            let lat_style =
                Style::default().fg(style::at_opacity(style::heat_color(r.lat as f64), 0.5));
            // Driver column = chars the human typed this turn (u_ch).
            // Tool-loop continuations have u_ch=0 — render dimly so the
            // human-driven turns visually pop.
            let drv_style = if r.u_ch > 0 {
                Style::default().fg(style::CYAN)
            } else {
                style::dim()
            };
            // Bot column = output tokens. Always present when there's a
            // response. Pink for visual parity with the chart's bot line.
            let bot_style = if r.out > 0 {
                Style::default().fg(style::PINK)
            } else {
                style::dim()
            };
            rows.push(Row::new(vec![
                Cell::from(Span::styled(when, style::label())),
                Cell::from(Span::styled(short_n(r.u_ch), drv_style)),
                Cell::from(Span::styled(short_n(r.out), bot_style)),
                Cell::from(Span::styled(style::fmt_lat(r.lat), lat_style)),
            ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(7),
        ],
    )
    .header(header);
    f.render_widget(table, inner);
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
