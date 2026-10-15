//! MODELS panel — list with online dots.

use crate::tui::style;
use crate::tui::App;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let inner = style::panel(f, area, "models");

    let mut toks: std::collections::HashMap<&String, u64> = std::collections::HashMap::new();
    for r in &app.agg.records {
        if let Some(m) = &r.model {
            *toks.entry(m).or_insert(0) += r.tot;
        }
    }
    let total: u64 = toks.values().sum();
    let mut sorted: Vec<(&String, u64)> = toks.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let avail = inner.width as usize;
    let name_width = avail.saturating_sub(6);

    let mut lines: Vec<Line> = Vec::new();
    let max_rows = (inner.height as usize).min(7);
    for (model, count) in sorted.iter().take(max_rows) {
        let pct = if total > 0 {
            *count as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        let dot = if *count > 0 {
            Span::styled("◉", Style::default().fg(style::LIME))
        } else {
            Span::styled("⊙", Style::default().fg(style::GREY))
        };
        let mut name = (**model).clone();
        if name.chars().count() > name_width {
            name = name.chars().take(name_width).collect();
        }
        let padded = format!("{:<width$}", name, width = name_width);
        lines.push(Line::from(vec![
            dot,
            Span::raw(" "),
            Span::styled(padded, style::label()),
            Span::styled(format!("{:>3}%", pct as u32), style::value()),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("(no traffic)", style::dim())));
    }

    f.render_widget(Paragraph::new(lines), inner);
}
