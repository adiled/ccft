//! SYSTEM panel — corner-accent chrome, no full border.

use crate::tui::style;
use crate::tui::App;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let inner = style::panel(f, area, "system");

    let port = app.cfg.port;
    let bound = port_bound(port);
    let port_dot = if bound { dot_on() } else { dot_off() };
    let ledger_dot = if app.cfg.ledger_enabled {
        dot_on()
    } else {
        dot_off()
    };

    let lines: Vec<Line> = vec![
        Line::from(vec![
            port_dot,
            Span::raw(" "),
            Span::styled(format!("{}", port), style::label()),
            Span::raw("  "),
            Span::styled("port", style::dim()),
        ]),
        Line::from(vec![
            ledger_dot,
            Span::raw(" "),
            Span::styled("ledger", style::label()),
        ]),
        Line::from(vec![
            Span::styled("⌁", Style::default().fg(style::PINK)),
            Span::raw(" "),
            Span::styled(app.agg.n.to_string(), style::label()),
            Span::raw(" "),
            Span::styled("reqs", style::dim()),
        ]),
        Line::from(vec![
            Span::styled("⌖", Style::default().fg(style::PINK)),
            Span::raw(" "),
            Span::styled(app.agg.sessions.len().to_string(), style::label()),
            Span::raw(" "),
            Span::styled(
                if app.agg.sessions.len() == 1 {
                    "session"
                } else {
                    "sessions"
                },
                style::dim(),
            ),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn dot_on() -> Span<'static> {
    Span::styled("◉", Style::default().fg(style::LIME))
}

fn dot_off() -> Span<'static> {
    Span::styled("⊙", Style::default().fg(style::GREY))
}

fn port_bound(port: u16) -> bool {
    use std::net::TcpStream;
    use std::time::Duration;
    let addr = format!("127.0.0.1:{}", port);
    if let Ok(a) = addr.parse() {
        TcpStream::connect_timeout(&a, Duration::from_millis(50)).is_ok()
    } else {
        false
    }
}
