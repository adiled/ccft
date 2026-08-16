//! Full-screen TUI — the default `ccft` invocation.
//!
//! Brainrot is the frame. Everything is one screen with brainrot at the
//! center. Time-dimension dials live on the keybar; overlays for sessions
//! and perf dissolve back to the main view on Esc.
//!
//! Architecture:
//!   - App holds dial state (range, bucket, stride) + cached aggregate.
//!   - On every event tick we re-pull from the ledger if the active range
//!     includes "now" or the user dialed.
//!   - Render is pure: state → frame.

mod chrome;
mod panels;
mod style;

use crate::brainrot::aggregate::{Aggregate, Baseline};
use crate::config::Config;
use crate::ledger_read::{iter_records, parse_range, Range};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Sessions,
    Perf,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RangePreset {
    Today,
    Yesterday,
    H1,
    H2,
    H4,
    H6,
    H12,
    H24, // = 1d, label "1d"
    D3,
    Week, // = 7d
    ThisWeek,
    W2,
    Mo1,
    All,
}

impl RangePreset {
    /// Linear order for cycling — granularity ascending.
    const ORDER: &'static [RangePreset] = &[
        RangePreset::Today,
        RangePreset::Yesterday,
        RangePreset::H1,
        RangePreset::H2,
        RangePreset::H4,
        RangePreset::H6,
        RangePreset::H12,
        RangePreset::H24,
        RangePreset::D3,
        RangePreset::Week,
        RangePreset::ThisWeek,
        RangePreset::W2,
        RangePreset::Mo1,
        RangePreset::All,
    ];

    fn cycle_next(self) -> Self {
        let i = Self::ORDER.iter().position(|p| *p == self).unwrap_or(0);
        Self::ORDER[(i + 1) % Self::ORDER.len()]
    }

    fn cycle_prev(self) -> Self {
        let i = Self::ORDER.iter().position(|p| *p == self).unwrap_or(0);
        Self::ORDER[(i + Self::ORDER.len() - 1) % Self::ORDER.len()]
    }

    fn spec(self) -> &'static str {
        match self {
            RangePreset::Today => "today",
            RangePreset::Yesterday => "yesterday",
            RangePreset::H1 => "1h",
            RangePreset::H2 => "2h",
            RangePreset::H4 => "4h",
            RangePreset::H6 => "6h",
            RangePreset::H12 => "12h",
            RangePreset::H24 => "24h",
            RangePreset::D3 => "3d",
            RangePreset::Week => "7d",
            RangePreset::ThisWeek => "this-week",
            RangePreset::W2 => "2w",
            RangePreset::Mo1 => "1mo",
            RangePreset::All => "all",
        }
    }
}

pub struct App {
    pub cfg: Config,
    pub range_preset: RangePreset,
    pub range: Range,
    pub agg: Aggregate,
    pub baseline: Baseline,
    pub overlay: Overlay,
    pub started: Instant,
    pub last_refresh: Instant,
    pub running: bool,
}

impl App {
    fn new() -> Self {
        // --dev TUI flips CCFT_DEV (set in main.rs), so Config::load()
        // already resolves to dev.json (port 7179) and the ledger reads land
        // on the dev ledger. Non-dev loads the production config.
        let cfg = Config::load();
        let preset = RangePreset::Today;
        let range = parse_range(preset.spec()).unwrap_or(Range {
            since: 0.0,
            until: 0.0,
            label: "today".into(),
        });
        let agg = Aggregate::ingest(iter_records(Some(range.since), Some(range.until)));
        // Baseline: full ledger, used for self-normalized z-scoring.
        let baseline_records: Vec<_> = iter_records(None, None).collect();
        let baseline = Baseline::from_records(&baseline_records);
        Self {
            cfg,
            range_preset: preset,
            range,
            agg,
            baseline,
            overlay: Overlay::None,
            started: Instant::now(),
            last_refresh: Instant::now(),
            running: true,
        }
    }

    fn refresh(&mut self) {
        let r = parse_range(self.range_preset.spec()).unwrap_or_else(|_| self.range.clone());
        self.range = r;
        self.agg = Aggregate::ingest(iter_records(Some(self.range.since), Some(self.range.until)));
        // Refresh baseline on every tick — the ledger may have grown since
        // last refresh and the baseline should always reflect the full
        // current history. Cheap on disk reads.
        let baseline_records: Vec<_> = iter_records(None, None).collect();
        self.baseline = Baseline::from_records(&baseline_records);

        // For "all", snap the range start to the first actual record timestamp
        // so the x-axis doesn't span 1970-to-now uselessly. If there are no
        // records yet (brand-new install), fall back to "last 24h" so the
        // chart shows a reasonable empty range instead of a 55-year span.
        if self.range_preset == RangePreset::All {
            if let Some(first) = self.agg.first_ts {
                self.range.since = first;
            } else {
                self.range.since = self.range.until - 86400.0;
            }
        }

        self.last_refresh = Instant::now();
    }

    fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // Overlay-specific keys first
        if self.overlay != Overlay::None {
            match code {
                KeyCode::Esc => {
                    self.overlay = Overlay::None;
                    return;
                }
                _ => {}
            }
        }
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.running = false,
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => self.running = false,
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Right | KeyCode::Char(']') => {
                self.range_preset = self.range_preset.cycle_next();
                self.refresh();
            }
            KeyCode::Left | KeyCode::Char('[') => {
                self.range_preset = self.range_preset.cycle_prev();
                self.refresh();
            }
            KeyCode::Char('t') => {
                self.range_preset = RangePreset::Today;
                self.refresh();
            }
            KeyCode::Char('y') => {
                self.range_preset = RangePreset::Yesterday;
                self.refresh();
            }
            KeyCode::Char('w') => {
                self.range_preset = RangePreset::Week;
                self.refresh();
            }
            KeyCode::Char('W') => {
                self.range_preset = RangePreset::ThisWeek;
                self.refresh();
            }
            KeyCode::Char('a') => {
                self.range_preset = RangePreset::All;
                self.refresh();
            }
            KeyCode::Char('h') => {
                self.range_preset = RangePreset::H24;
                self.refresh();
            }
            KeyCode::Char('s') => self.overlay = Overlay::Sessions,
            KeyCode::Char('p') => self.overlay = Overlay::Perf,
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            _ => {}
        }
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Resolve the local UTC offset NOW on the main thread before anything
    // else. time-rs's local_offset has a known soundness bug — calls
    // non-thread-safe libc TZ functions and can SIGABRT in MT contexts.
    // OnceLock-cached after this point.
    let _ = crate::tui::style::local_offset();

    // Enable mouse capture so the terminal can't scroll past our alt-screen.
    // Without this, some terminal apps (iTerm2, Terminal.app with certain
    // settings) honour wheel events on the main buffer even from alt screen,
    // letting the user scroll past the top of the TUI. Capturing the events
    // routes them to us and the terminal stops translating them to scroll.
    let _ = execute!(std::io::stdout(), EnableMouseCapture);

    // ratatui::run handles raw-mode + alt-screen + panic-hook + restore for us.
    // Our job is the event loop body.
    let result: Result<(), Box<dyn std::error::Error>> = ratatui::run(|terminal| {
        let mut app = App::new();
        let tick = Duration::from_millis(1000);
        loop {
            // Phase 7 — feed the elapsed time into style's thread_local so
            // paint_signal can drift the shimmer phase between frames.
            let elapsed = app.started.elapsed().as_secs_f32();
            crate::tui::style::set_time_offset(elapsed);

            terminal.draw(|f| draw(f, &app))?;

            let timeout = tick
                .checked_sub(app.last_refresh.elapsed())
                .unwrap_or(Duration::from_millis(50));
            if event::poll(timeout)? {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press {
                        app.handle_key(k.code, k.modifiers);
                    }
                }
            } else if app.last_refresh.elapsed() >= tick {
                // periodic re-aggregate — only when range includes "now"
                if app.range.until >= crate::ledger_read::now_secs() - 5.0 {
                    app.refresh();
                }
                app.last_refresh = Instant::now();
            }
            if !app.running {
                return Ok(());
            }
        }
    });

    // Mirror the enter_run setup: release mouse capture on exit so the user's
    // terminal returns to normal scroll behaviour.
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    result
}

fn draw(f: &mut ratatui::Frame, app: &App) {
    use ratatui::layout::{Constraint, Direction, Layout};

    let area = f.area();

    // Substrate: very dark navy (#02050E, RGB 2/5/14). Just shy of pure black
    // so the dim end of the energy field has a hue-matched ground to fade
    // into rather than terminal black. Painted first so every cell starts
    // from this exact color, then phosphor noise perturbs each cell ±3 RGB
    // for "old display memory" feel.
    use ratatui::style::{Color as RColor, Style as RStyle};
    use ratatui::widgets::Block;
    f.render_widget(
        Block::default().style(RStyle::default().bg(RColor::Rgb(0x02, 0x05, 0x0e))),
        area,
    );

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(0),    // body
            Constraint::Length(1), // keybar
        ])
        .split(area);

    chrome::header(f, outer[0], app);
    body(f, outer[1], app);
    chrome::keybar(f, outer[2], app);

    if app.overlay != Overlay::None {
        panels::overlay::render(f, area, app);
    }
}

fn body(f: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &App) {
    use ratatui::layout::{Constraint, Direction, Layout};

    // Three columns: left rail / center / right ledger.
    //
    // Ratios per the layout brief: 14% / 64% / 22%. The center column's
    // geometric mid-point lands at 14% + 64%/2 = 46% of screen, which is
    // 4% LEFT of screen mid (50%). That's the intentional left bias —
    // composition reads as broadcast/surveillance rather than enterprise
    // grid. Perfect centering kills the cinematic feel.
    //
    // Uniform 1-cell gutter on both axes. Terminal cells are ~2x taller
    // than wide so a vertical 1-row gap reads visually larger than a
    // horizontal 1-column gap, but logically they're both "1 cell of
    // gutter" between adjacent widgets.
    //
    //   left rail (14%)   = SYSTEM / MODELS / HEAT / STREAM   "machine room"
    //   center    (64%)   = BRAINROT / METRICS / DIAGNOSTICS  cinematic
    //   right     (22%)   = LEDGER (full height)              "surveilling"
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .spacing(1)
        .constraints([
            Constraint::Percentage(14),
            Constraint::Percentage(64),
            Constraint::Percentage(22),
        ])
        .split(area);

    // Left column: SYSTEM / MODELS / HEAT.
    // HEAT absorbs the freed STREAM space (was 44%) so the heat-by-time
    // bar chart has room to breathe and the panel reads as "machine room
    // density" by itself.
    let left = Layout::default()
        .direction(Direction::Vertical)
        .spacing(1)
        .constraints([
            Constraint::Percentage(18),
            Constraint::Percentage(24),
            Constraint::Percentage(58),
        ])
        .split(cols[0]);

    panels::system::render(f, left[0], app);
    panels::models::render(f, left[1], app);
    panels::heat::render(f, left[2], app);

    // Center column: BRAINROT GRAPH / METRICS STRIP / DIAGNOSTICS.
    let center = Layout::default()
        .direction(Direction::Vertical)
        .spacing(1)
        .constraints([
            Constraint::Percentage(45),
            Constraint::Percentage(18),
            Constraint::Percentage(37),
        ])
        .split(cols[1]);

    panels::brainrot::render(f, center[0], app);
    panels::metrics::render(f, center[1], app);
    panels::diagnosis::render(f, center[2], app);

    // Right column: LEDGER (full height — vertical, surveilling).
    panels::ledger::render(f, cols[2], app);
}
