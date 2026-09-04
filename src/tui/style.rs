//! TUI palette + small chrome helpers (panel title pip, corner accents).
//! Outlines are replaced with accents — no Block borders anywhere.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use std::sync::OnceLock;

/// Cached local UTC offset. `time::UtcOffset::current_local_offset()` calls
/// non-thread-safe libc TZ functions and can SIGABRT on macOS in MT contexts.
/// We resolve it ONCE at startup (single-threaded) and reuse forever.
pub fn local_offset() -> time::UtcOffset {
    static V: OnceLock<time::UtcOffset> = OnceLock::new();
    *V.get_or_init(|| time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC))
}

// ─── Palette ──────────────────────────────────────────────────────────────────
//
// Five neon hues + grey + white on pure black. Each metric/category carries a
// single canonical hue used everywhere it appears (label, line, tile, tag).

pub const PINK: Color = Color::Rgb(0xff, 0x1f, 0x8c); // brand / titles / COPE
pub const CYAN: Color = Color::Rgb(0x00, 0xe5, 0xff); // body / DELUSION / values
pub const GOLD: Color = Color::Rgb(0xff, 0xd6, 0x00); // SCHIZO / warn
pub const LIME: Color = Color::Rgb(0x39, 0xff, 0x14); // SILLINESS / online / OK
pub const VIOLET: Color = Color::Rgb(0xa8, 0x55, 0xf7); // DOOMER / accent series
pub const WHITE: Color = Color::Rgb(0xe6, 0xe9, 0xee); // numeric / readouts
pub const GREY: Color = Color::Rgb(0x3f, 0x44, 0x52); // off / scaffold
pub const SUBTLE: Color = Color::Rgb(0x6b, 0x72, 0x80); // dim body, separators

// Border accent: a muted, hue-preserved pink used for panel chrome only.
// Distinct from the bright brand PINK so the chrome reads as a translucent
// seam rather than a brand statement.

// Frame background — RGB(2, 5, 14) / #02050E. The exact substrate the frame
// is painted with; energy-fade math lerps toward this so dim cells dissolve
// cleanly into the ground instead of through a different shade of black.
pub const BG: Color = Color::Rgb(0x02, 0x05, 0x0e);

// Backward-compatibility aliases used by panels written before the repaint.
/// Signal tile color by sigma-bucket severity.
pub fn signal_color(bucket: u8) -> Color {
    match bucket {
        1 | 2 => LIME,
        3 => GOLD,
        4 => PINK,
        _ => Color::Rgb(0xff, 0x44, 0x66),
    }
}

pub fn title() -> Style {
    Style::default().fg(PINK).add_modifier(Modifier::BOLD)
}

pub fn brand() -> Style {
    Style::default().fg(PINK).add_modifier(Modifier::BOLD)
}

pub fn label() -> Style {
    Style::default().fg(SUBTLE)
}

pub fn value() -> Style {
    Style::default().fg(WHITE)
}

pub fn dim() -> Style {
    Style::default().fg(GREY)
}

#[allow(dead_code)]
pub fn good() -> Style {
    Style::default().fg(LIME)
}

#[allow(dead_code)]
pub fn warn() -> Style {
    Style::default().fg(GOLD)
}

#[allow(dead_code)]
pub fn alert() -> Style {
    Style::default().fg(PINK)
}

pub fn key_hint() -> Style {
    Style::default().fg(PINK)
}

pub fn score_color(score: u32) -> Color {
    if score < 40 {
        LIME
    } else if score < 70 {
        GOLD
    } else {
        PINK
    }
}

pub fn heat_color(latency_ms: f64) -> Color {
    if latency_ms < 500.0 {
        SUBTLE
    } else if latency_ms < 1500.0 {
        LIME
    } else if latency_ms < 3000.0 {
        CYAN
    } else if latency_ms < 6000.0 {
        GOLD
    } else {
        PINK
    }
}

// ─── Panel chrome: L-shaped corner accents with gradient fade to nothing ─────
//
// Each panel renders four L-brackets, one at each corner. Each L has a long
// horizontal arm and a short vertical arm. The first cell at the actual corner
// is brightest PINK; cells along the arm fade RGB-linearly to fully transparent
// (we just stop painting past the threshold, leaving terminal background).
// Mid-edge is empty space — the panels are delineated by partial corners +
// spatial separation, not continuous borders.
//
// Title pip lives at row 0, col 2: " // LABEL " with `// ` in dim white and
// LABEL in pink bold.

// Border = a low-resolution light field around each panel's perimeter.
//
// The model is intentionally simple: every cell rolls a weighted random
// luminance value. Most cells land in the mid range (0.32-0.55). A small
// fraction (~4%) become BURSTS (luminance 0.85-1.0, brightened in their
// own hue family — never washed out to white). Corners are always bursts.
// Some bursts noise-corrupt an adjacent cell to near-zero opacity; that
// gives the line its "imperfect display" character. A slow time-driven
// drift modulates burst selection so individual cells breathe in and out
// of overload over ~16 second cycles without animating geometry.
//
// HUE comes from a separate slow sine drift between polluted magenta and
// polluted cyan, so the perimeter passes through long magenta zones and
// long cyan zones with brief purple in the transitions.
//
// What this model NO LONGER has (deliberately): no momentum, no bleed
// kernels, no pressure curve, no hotspot peak-detection, no contamination
// pass, no phosphor flare pass, no named decorative effects. Past
// iterations layered "effects" on top of a smooth signal, which made the
// border read as "renderer narrating its mechanism." This is a single
// weighted distribution.

// Hue field — wavelength controls how often cyan↔magenta alternates along
// the perimeter. Short wavelength (40) means most panels see 2-4 distinct
// color zones rather than one big zone of each color. The yin-yang cycling
// is the dominant visual rhythm, independent of luminance / bursts /
// dead zones (those layers all multiply by the cell's natural hue color).
const LAMBDA_HUE: f32 = 40.0;

// Luminance distribution weights.
const BURST_RATE: f32 = 0.020; // per-cell start probability — ~1-2 events per panel
const BURST_DOUBLE_RATE: f32 = 0.22; // 22% of bursts span 2 cells
const BURST_TRIPLE_RATE: f32 = 0.08; // 8% of bursts span 3 cells (rest are single)
const DEAD_RATE: f32 = 0.10; // ~10% of eligible cells are dead-zone cells
const NOISE_AT_BURST: f32 = 0.30; // 30% of bursts noise-corrupt an adjacent cell
const MID_LUM_MIN: f32 = 0.32;
const MID_LUM_MAX: f32 = 0.55;
const BURST_LUM_MIN: f32 = 0.85;
const BURST_LUM_MAX: f32 = 1.00;
const NOISE_LUM: f32 = 0.04;

// Dead-tier intensity is itself weighted random: 70% land in the EXPECTED
// moderate-dim band, 30% in the deeply-dim outlier band — same shape as
// the burst tier (most are normal, a few are extreme).
const DEAD_DEEP_RATE: f32 = 0.30;
const DEAD_MODERATE_MIN: f32 = 0.10;
const DEAD_MODERATE_MAX: f32 = 0.18;
const DEAD_DEEP_MIN: f32 = 0.02;
const DEAD_DEEP_MAX: f32 = 0.08;

// Cells within this many positions of a corner cannot be DEAD. Corners
// always read as the panel's luminance high — no dim spans next to them.
const CORNER_DEAD_EXCLUSION: i32 = 2;

// ─── Dead-zone stretches (flying duck) ───────────────────────────────────────
//
// A contiguous span of cells gets a dimming multiplier applied AFTER tier
// assignment. What would have fallen still falls, just as residue: bursts
// remain bursts in CHARACTER (their color is still brightened in-hue) but
// their luminance attenuates. Mid cells drop into the substrate-quiet
// range; per-cell DEAD bits inside a flying stretch get dimmer still.
//
// Both occurrence AND intensity follow weighted random: per-cell start
// probability, weighted random length (70% short, 30% longer), weighted
// random dim factor (70% moderate, 30% deep). Stretches never enter the
// corner-protected band — flying duck doesn't fly over corners.
const DEAD_ZONE_START_RATE: f32 = 0.018; // ~1-2 zones per panel of ~70 cells
const DEAD_ZONE_DEEP_RATE: f32 = 0.30;
const DEAD_ZONE_LENGTH_LONG_RATE: f32 = 0.30;
const DEAD_ZONE_SHORT_MIN: usize = 3;
const DEAD_ZONE_SHORT_MAX: usize = 6;
const DEAD_ZONE_LONG_MIN: usize = 7;
const DEAD_ZONE_LONG_MAX: usize = 12;
const DEAD_ZONE_DIM_MOD_MIN: f32 = 0.35;
const DEAD_ZONE_DIM_MOD_MAX: f32 = 0.55;
const DEAD_ZONE_DIM_DEEP_MIN: f32 = 0.15;
const DEAD_ZONE_DIM_DEEP_MAX: f32 = 0.25;

// Polluted neon — never pure #ff00ff/#00ffff. Magenta is red-tinged, cyan is
// blue-tinged. Slightly desaturated so they don't scream.
const NEON_MAGENTA: Color = Color::Rgb(0xe2, 0x2a, 0x78);
const NEON_CYAN: Color = Color::Rgb(0x2c, 0xa6, 0xc4);

pub fn panel(f: &mut Frame, area: Rect, label: &str) -> Rect {
    if area.width < 4 || area.height < 3 {
        return area;
    }
    paint_corner_accents(f.buffer_mut(), area);

    // Title pip: " // LABEL " in row 0, starting at col 2. Empty label =
    // chrome only (used for sub-panels like the BRAINROT score-tile row).
    if !label.is_empty() {
        let pip_x = area.x.saturating_add(2);
        if pip_x < area.x + area.width {
            let prefix = "// ";
            let label_uc = label.to_uppercase();
            let pip_w = (prefix.chars().count() + label_uc.chars().count() + 2)
                .min((area.x + area.width).saturating_sub(pip_x) as usize)
                as u16;
            let title_rect = Rect {
                x: pip_x,
                y: area.y,
                width: pip_w,
                height: 1,
            };
            let line = Line::from(vec![
                Span::raw(" "),
                Span::styled(prefix, Style::default().fg(SUBTLE)),
                Span::styled(label_uc, title()),
                Span::raw(" "),
            ]);
            f.render_widget(Paragraph::new(line), title_rect);
        }
    }

    Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

/// Energized perimeter ribbon. The panel border is rendered as a sampled
/// 1D signal that wraps clockwise around the rectangle — one continuous
/// loop, not four separate edges. Every cell paints; brightness and hue
/// come from smooth low-frequency analog fields plus a post-pass
/// exponential-moving-average smoother that gives the line *inertia*
/// (energy persists across nearby cells, so streaks don't atomize).
fn paint_corner_accents(buf: &mut Buffer, area: Rect) {
    let seed = panel_label_seed(buf.area, area);
    paint_panel_border(buf, area, &seed);
}

/// Generic energized-line painter for any 1D path of cells. Used by tile
/// dividers and other elements that want the perimeter rail look but NOT
/// the substrate halo (those would bleed into adjacent content).
/// Panel border = perimeter signal + a single subliminal substrate halo.
/// All variation is one weighted-random luminance + a slow hue field.
fn paint_panel_border(buf: &mut Buffer, area: Rect, seed: &str) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let cells = collect_perimeter(area);
    let n = cells.len();
    if n < 4 {
        return;
    }
    let t = time_offset();
    let hue = compute_hue_field(n, seed);
    let (luminance, is_burst) = compute_perimeter_luminance(&cells, seed, t);

    paint_cells_with_signal(buf, &cells, &hue, &luminance, &is_burst, seed);
}

// ─── Hue field ───────────────────────────────────────────────────────────────
//
// One slow-wavelength sine drift between polluted magenta (~0) and polluted
// cyan (~1). Wavelength tuned so a typical panel perimeter passes through
// 2-4 distinct color zones — frequent yin-yang cycling, not one-big-zone-
// of-each-color. NO temporal drift on hue: zones stay stable across frames
// so the eye reads the panel's color identity instead of chasing motion.
//
// Sharpening: tanh(h × 2.2) pushes the sine values toward the rails so
// each zone reads clearly as cyan or magenta with brief purple only at
// the crossings. Light cyclic smoothing follows so cycles don't atomize
// into salt-and-pepper.

fn compute_hue_field(n: usize, seed: &str) -> Vec<f32> {
    let p_hue = phase_from(seed, "hue");
    let mut hue: Vec<f32> = Vec::with_capacity(n);
    for i in 0..n {
        let p = i as f32;
        let h = (p * std::f32::consts::TAU / LAMBDA_HUE + p_hue).sin();
        let sharpened = (h * 2.2).tanh();
        hue.push((sharpened + 1.0) * 0.5);
    }
    // Light cyclic smoothing — enough to remove micro-jaggies without
    // washing out the short-wavelength cycles. One pass at high alpha
    // (cell weight 0.65, neighbor 0.35) instead of the previous heavy
    // two-pass smoothing that flattened the alternation.
    {
        let mut last = hue[n - 1];
        for i in 0..n {
            let v = hue[i] * 0.65 + last * 0.35;
            hue[i] = v;
            last = v;
        }
    }
    hue
}

// ─── Perimeter luminance: weighted random distribution ───────────────────────
//
// Per-cell weighted random sampling with slow temporal drift. The ENTIRE
// luminance variation of the border:
//
//   - BURST_RATE (~4%):       luminance ∈ [0.85, 1.0]   ← visibly overdriven
//   - DEAD_RATE  (~10%):      luminance in dead-tier    ← dead-zone cells
//   - rest      (~86%):       luminance ∈ [0.32, 0.55]  ← perceptible mid
//   - CORNERS + ±2 neighbors: always BURST / never DEAD  ← corners pop
//
// DEAD intensity is itself weighted random:
//   - 70% of dead cells:  moderate-dim ∈ [0.10, 0.18]   ← expected
//   - 30% of dead cells:  deeply-dim ∈ [0.02, 0.08]     ← outlier
//
// For each BURST cell, a noise pass: NOISE_AT_BURST (~30%) of bursts
// noise-corrupt one adjacent non-burst, non-corner cell to NOISE_LUM
// (~0.04 opacity). The "imperfect display" texture.
//
// Time drift modulates each cell's burst-roll AND dead-roll by ±0.04 with
// a per-cell phase, so individual cells drift in and out of both states
// over ~16s cycles. Geometry is never animated; bright AND dim cells
// both breathe.

fn compute_perimeter_luminance(
    cells: &[(u16, u16, char)],
    seed: &str,
    time_offset: f32,
) -> (Vec<f32>, Vec<bool>) {
    let n = cells.len();
    let mut lum = vec![0.0_f32; n];
    let mut is_burst = vec![false; n];

    // Pre-pass — build the corner-adjacency mask. Cells within
    // ±CORNER_DEAD_EXCLUSION positions of any corner are protected from
    // the DEAD tier so corner approaches always read bright.
    let mut near_corner = vec![false; n];
    for i in 0..n {
        if matches!(cells[i].2, '╭' | '╮' | '╰' | '╯') {
            for d in -CORNER_DEAD_EXCLUSION..=CORNER_DEAD_EXCLUSION {
                let j = ((i as i32 + d).rem_euclid(n as i32)) as usize;
                near_corner[j] = true;
            }
        }
    }

    // Pass 1 — per-cell weighted random over BURST / DEAD / MID.
    //
    // BURST is a STATEFUL event: when one starts at cell i, it consumes a
    // weighted-random number of cells (1, 2, or 3) before releasing. Each
    // cell of the stretch gets its own luminance roll within [BURST_LUM_MIN,
    // BURST_LUM_MAX]. Corners are always single-cell bursts and interrupt
    // any active stretch.
    let mut active_burst: i32 = 0;
    for i in 0..n {
        let (x, y, gi) = cells[i];
        let is_corner = matches!(gi, '╭' | '╮' | '╰' | '╯');
        let h = cell_hash(seed, x, y);
        let detail = ((h >> 32) & 0xFFFF) as f32 / 0xFFFF as f32;

        // Corners — always single-cell burst, end any active stretch.
        if is_corner {
            lum[i] = BURST_LUM_MIN + detail * (BURST_LUM_MAX - BURST_LUM_MIN);
            is_burst[i] = true;
            active_burst = 0;
            continue;
        }

        // Continue active burst stretch.
        if active_burst > 0 {
            lum[i] = BURST_LUM_MIN + detail * (BURST_LUM_MAX - BURST_LUM_MIN);
            is_burst[i] = true;
            active_burst -= 1;
            continue;
        }

        // Roll for new burst start (with time-drift breathing).
        let burst_static = (h & 0xFFFF) as f32 / 0xFFFF as f32;
        let burst_phase = ((h >> 16) & 0xFFFF) as f32 / 0xFFFF as f32 * std::f32::consts::TAU;
        let burst_drift = (time_offset * 0.4 + burst_phase).sin() * 0.04;
        let burst_roll = (burst_static + burst_drift).clamp(0.0, 1.0);

        if burst_roll < BURST_RATE {
            // Length — weighted random. Most are single-cell; a few are
            // double; a smaller fraction triple. Variability without
            // becoming streaks.
            let length_roll = ((h >> 48) & 0xFFFF) as f32 / 0xFFFF as f32;
            let length: i32 = if length_roll < BURST_TRIPLE_RATE {
                3
            } else if length_roll < BURST_TRIPLE_RATE + BURST_DOUBLE_RATE {
                2
            } else {
                1
            };
            lum[i] = BURST_LUM_MIN + detail * (BURST_LUM_MAX - BURST_LUM_MIN);
            is_burst[i] = true;
            active_burst = length - 1;
            continue;
        }

        // Dead roll — independent hash, independent time-drift phase.
        // Eligible only when the cell is NOT a corner-adjacency cell.
        if !near_corner[i] {
            let dh = cell_hash(seed, x.wrapping_add(31), y.wrapping_add(37));
            let dead_static = (dh & 0xFFFF) as f32 / 0xFFFF as f32;
            let dead_phase = ((dh >> 16) & 0xFFFF) as f32 / 0xFFFF as f32 * std::f32::consts::TAU;
            let dead_drift = (time_offset * 0.4 + dead_phase).sin() * 0.04;
            let dead_roll = (dead_static + dead_drift).clamp(0.0, 1.0);

            if dead_roll < DEAD_RATE {
                // Intensity sub-roll: 70% moderate-dim, 30% deeply-dim.
                let intensity_roll = ((dh >> 32) & 0xFFFF) as f32 / 0xFFFF as f32;
                let intensity_detail = ((dh >> 48) & 0xFFFF) as f32 / 0xFFFF as f32;
                lum[i] = if intensity_roll < DEAD_DEEP_RATE {
                    DEAD_DEEP_MIN + intensity_detail * (DEAD_DEEP_MAX - DEAD_DEEP_MIN)
                } else {
                    DEAD_MODERATE_MIN + intensity_detail * (DEAD_MODERATE_MAX - DEAD_MODERATE_MIN)
                };
                continue;
            }
        }

        // Default — MID tier.
        lum[i] = MID_LUM_MIN + detail * (MID_LUM_MAX - MID_LUM_MIN);
    }

    // Pass 2 — burst-triggered noise corruption.
    for i in 0..n {
        if !is_burst[i] {
            continue;
        }
        let (x, y, _) = cells[i];
        let h = cell_hash(seed, x.wrapping_add(7919), y.wrapping_add(7907));
        let noise_roll = (h & 0xFFFF) as f32 / 0xFFFF as f32;
        if noise_roll < NOISE_AT_BURST {
            let direction_left = ((h >> 16) & 1) == 0;
            let neighbor = if direction_left {
                (i + n - 1) % n
            } else {
                (i + 1) % n
            };
            let (_, _, gj) = cells[neighbor];
            let neighbor_corner = matches!(gj, '╭' | '╮' | '╰' | '╯');
            if !is_burst[neighbor] && !neighbor_corner {
                lum[neighbor] = NOISE_LUM;
            }
        }
    }

    // Pass 3 — dead-zone stretches (flying duck). Stateful walk through
    // the perimeter: at each cell, if no zone is active, roll for "start
    // a new zone." If active, apply the zone's dimming to luminance.
    // Zones get cut short by corner-protected bands. is_burst is NOT
    // touched — the dropping pattern stays; only the visibility falls.
    let mut active_remaining: i32 = 0;
    let mut active_dim: f32 = 1.0;
    for i in 0..n {
        if active_remaining > 0 {
            if near_corner[i] {
                active_remaining = 0; // zone interrupted by corner band
            } else {
                lum[i] *= active_dim;
                active_remaining -= 1;
            }
            continue;
        }
        if near_corner[i] {
            continue;
        }
        let (x, y, _) = cells[i];

        // Start roll — independent hash, time-drifted so zones breathe.
        let h = cell_hash(seed, x.wrapping_add(2017), y.wrapping_add(2027));
        let static_roll = (h & 0xFFFF) as f32 / 0xFFFF as f32;
        let phase = ((h >> 16) & 0xFFFF) as f32 / 0xFFFF as f32 * std::f32::consts::TAU;
        let drift = (time_offset * 0.3 + phase).sin() * 0.012;
        let start_roll = (static_roll + drift).clamp(0.0, 1.0);

        if start_roll < DEAD_ZONE_START_RATE {
            // Length — weighted random, 70% short / 30% long.
            let length_roll = ((h >> 32) & 0xFFFF) as f32 / 0xFFFF as f32;
            let length_detail = ((h >> 48) & 0xFFFF) as f32 / 0xFFFF as f32;
            let (lmin, lmax) = if length_roll < DEAD_ZONE_LENGTH_LONG_RATE {
                (DEAD_ZONE_LONG_MIN, DEAD_ZONE_LONG_MAX)
            } else {
                (DEAD_ZONE_SHORT_MIN, DEAD_ZONE_SHORT_MAX)
            };
            let length = (lmin + (length_detail * (lmax - lmin + 1) as f32) as usize).min(lmax);

            // Dim factor — weighted random, 70% moderate / 30% deep.
            let dh = cell_hash(seed, x.wrapping_add(3019), y.wrapping_add(3023));
            let dim_roll = (dh & 0xFFFF) as f32 / 0xFFFF as f32;
            let dim_detail = ((dh >> 16) & 0xFFFF) as f32 / 0xFFFF as f32;
            let dim_factor = if dim_roll < DEAD_ZONE_DEEP_RATE {
                DEAD_ZONE_DIM_DEEP_MIN
                    + dim_detail * (DEAD_ZONE_DIM_DEEP_MAX - DEAD_ZONE_DIM_DEEP_MIN)
            } else {
                DEAD_ZONE_DIM_MOD_MIN + dim_detail * (DEAD_ZONE_DIM_MOD_MAX - DEAD_ZONE_DIM_MOD_MIN)
            };

            lum[i] *= dim_factor;
            active_remaining = length as i32 - 1;
            active_dim = dim_factor;
        }
    }

    (lum, is_burst)
}

// ─── Paint perimeter cells ───────────────────────────────────────────────────
//
// Each cell renders its canonical glyph (`─ │ ╭ ╮ ╰ ╯`) at the luminance
// produced by `compute_perimeter_luminance`. BURST cells get a hue-preserved
// RGB brightening (channels scale up multiplicatively, clip at 255). The
// brightening keys off the is_burst FLAG, not a luminance threshold, so a
// burst inside a flying-duck dead zone — whose luminance has been dimmed —
// still gets the saturated burst COLOR. The dropping is still recognizable;
// only its visibility is reduced ("residue of the dropping").

fn paint_cells_with_signal(
    buf: &mut Buffer,
    cells: &[(u16, u16, char)],
    hue: &[f32],
    luminance: &[f32],
    is_burst: &[bool],
    seed: &str,
) {
    let n = cells.len();
    let p_temp = phase_from(seed, "temp");

    for (i, &(x, y, default_glyph)) in cells.iter().enumerate() {
        let lum = luminance[i];

        let p = i as f32;
        let temp_shift = (p * std::f32::consts::TAU / 80.0 + p_temp).sin() * 0.06;
        let h = (hue[i] + temp_shift).clamp(0.0, 1.0);
        let mut color = lerp_rgb(NEON_MAGENTA, NEON_CYAN, h);

        // Brightening keys off the flag, not lum — so dimmed bursts (in
        // flying-duck stretches) still carry the saturated burst color.
        // The brightening factor scales with the cell's UNDIMMED burst
        // intensity though; for a dimmed burst we approximate the original
        // by dividing out the visible lum's relationship to BURST_LUM_MIN.
        if is_burst[i] {
            // Use a fixed mid-burst brighten factor for dimmed bursts, or
            // the lum-scaled factor for full bursts. Branch on whether we
            // can recover the original lum from the visible value.
            let factor = if lum >= BURST_LUM_MIN {
                1.0 + (lum - BURST_LUM_MIN) / (BURST_LUM_MAX - BURST_LUM_MIN) * 0.55
            } else {
                // Dimmed burst — apply the burst-tier mid brightening so
                // the saturated color identity survives the dimming.
                1.30
            };
            color = brighten(color, factor);
        }

        let final_color = scale_color(color, lum);
        set_char(buf, x, y, default_glyph, fg(final_color));
    }

    // ── On-rail glow ────────────────────────────────────────────────────
    //
    // Each BURST cell tints its own background AND the backgrounds of its
    // same-edge ±1 / ±2 neighbors. The rail glyph stays drawn on top; the
    // tint is in the burst's hue, decaying with distance. Result: a 3-5
    // cell luminous zone ALONG THE RAIL at each burst — never floating in
    // off-rail substrate.
    //
    // Dimmed bursts (in flying-duck dead zones) suppress glow.
    let p_temp_glow = phase_from(seed, "temp");
    for i in 0..n {
        if !is_burst[i] {
            continue;
        }
        if luminance[i] < BURST_LUM_MIN * 0.80 {
            continue; // dimmed by dead zone — duck flying, no glow
        }

        // Burst's own hue (with the same temp jitter as the FG paint).
        let p = i as f32;
        let temp_shift = (p * std::f32::consts::TAU / 80.0 + p_temp_glow).sin() * 0.06;
        let h_self = (hue[i] + temp_shift).clamp(0.0, 1.0);
        let hue_color = lerp_rgb(NEON_MAGENTA, NEON_CYAN, h_self);

        // Distance-decayed tint amounts.
        let band = [
            (0i32, 0.22f32),
            (-1, 0.12),
            (1, 0.12),
            (-2, 0.05),
            (2, 0.05),
        ];
        let (xi, yi, _) = cells[i];
        for (offset, amount) in band {
            let j = ((i as i32 + offset).rem_euclid(n as i32)) as usize;
            let (xj, yj, _) = cells[j];
            // Same-edge only — burst glow doesn't bleed around corners.
            // (For the burst itself, offset=0, this is trivially true.)
            if xi != xj && yi != yj {
                continue;
            }
            // Lerp BG from substrate toward the burst's hue at `amount`.
            let bg_tinted = lerp_rgb(BG, hue_color, amount);
            if let Some(cell) = buf.cell_mut((xj, yj)) {
                cell.set_bg(bg_tinted);
            }
        }
    }
}

// ─── Phase 7: temporal phase offset ──────────────────────────────────────────

use std::cell::Cell as StdCell;
thread_local! {
    static TIME_OFFSET: StdCell<f32> = const { StdCell::new(0.0) };
}

/// Set the global per-frame time offset. Called from `tui::run` once per
/// frame with `app.started.elapsed().as_secs_f32()`. Read by the signal
/// computation to drift the shimmer phase over time.
pub fn set_time_offset(t: f32) {
    TIME_OFFSET.with(|c| c.set(t));
}

fn time_offset() -> f32 {
    TIME_OFFSET.with(|c| c.get())
}

/// Paint a rectangular signal-border around `area` — same thematics as
/// the panel chrome. Used for the active range chip's outline and any
/// other small bounded element that wants the full energized border.
/// Solid-color rounded-corner rectangle. Same geometry as `signal_rect`
/// (rounded `╭ ╮ ╰ ╯` corners + `─ │` edges) but painted in one uniform
/// color. Used for elements that want crisp button-style edges instead
/// of the energized streaky chrome — e.g. the active range chip.
/// Paint a vertical divider strip at column `x`, from `y` for `height`
/// cells. Same signal thematics as panel borders so dividers feel like
/// they belong to the same chrome family.
/// Walk the panel perimeter clockwise starting at top-left, returning the
/// cell coordinates and default glyph for each step. The returned list is
/// what the energy/hue signals are sampled against; treating it as a single
/// sequence (not four edges) is what lets streaks bleed across corners.
fn collect_perimeter(area: Rect) -> Vec<(u16, u16, char)> {
    let right = area.x + area.width - 1;
    let bottom = area.y + area.height - 1;
    let mut cells = Vec::new();

    cells.push((area.x, area.y, '╭'));
    for x in (area.x + 1)..right {
        cells.push((x, area.y, '─'));
    }
    cells.push((right, area.y, '╮'));
    for y in (area.y + 1)..bottom {
        cells.push((right, y, '│'));
    }
    cells.push((right, bottom, '╯'));
    for x in ((area.x + 1)..right).rev() {
        cells.push((x, bottom, '─'));
    }
    cells.push((area.x, bottom, '╰'));
    for y in ((area.y + 1)..bottom).rev() {
        cells.push((area.x, y, '│'));
    }
    cells
}

fn phase_from(seed: &str, tag: &str) -> f32 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    seed.hash(&mut h);
    tag.hash(&mut h);
    let v = h.finish();
    (v as f32 / u64::MAX as f32) * std::f32::consts::TAU
}

fn brighten(c: Color, factor: f32) -> Color {
    let (r, g, b) = rgb(c);
    Color::Rgb(
        ((r as f32 * factor) as u32).min(255) as u8,
        ((g as f32 * factor) as u32).min(255) as u8,
        ((b as f32 * factor) as u32).min(255) as u8,
    )
}

fn lerp_rgb(from: Color, to: Color, t: f32) -> Color {
    let (fr, fg, fb) = rgb(from);
    let (tr, tg, tb) = rgb(to);
    Color::Rgb(
        (fr as f32 + (tr as f32 - fr as f32) * t) as u8,
        (fg as f32 + (tg as f32 - fg as f32) * t) as u8,
        (fb as f32 + (tb as f32 - fb as f32) * t) as u8,
    )
}

/// Hash (label || x || y) → u64 using std SipHash. Stable within a process
/// run, randomized across runs — exactly what we want for "per-session
/// noise pattern that doesn't change between redraws".
fn cell_hash(label: &str, x: u16, y: u16) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    label.hash(&mut h);
    x.hash(&mut h);
    y.hash(&mut h);
    h.finish()
}

/// Take 16 bits of the hash starting at `shift`, scale to [0, 1).
/// Multiply an RGB color by a brightness factor. Channels clamp on the way
/// down (since we only scale by ≤ 1.0). Hue is preserved.
fn scale_color(c: Color, factor: f32) -> Color {
    let (r, g, b) = rgb(c);
    Color::Rgb(
        (r as f32 * factor) as u8,
        (g as f32 * factor) as u8,
        (b as f32 * factor) as u8,
    )
}

/// Public version of `scale_color` — multiply each channel by `opacity`
/// (0.0..1.0) to simulate translucent text. Hue preserved, channels clamped.
pub fn at_opacity(c: Color, opacity: f32) -> Color {
    scale_color(c, opacity.clamp(0.0, 1.0))
}

/// Format a millisecond latency for display: `<1000ms` keeps `Nms`,
/// `≥1000ms` collapses to `N.Ns`. One decimal place is enough at the
/// readability threshold the user sees in the dashboard.
pub fn fmt_lat(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// Build a per-panel seed string from the panel's position. Two panels at
/// different positions → different noise patterns; the same panel at the
/// same position → same noise across redraws.
fn panel_label_seed(_buf_area: Rect, panel_area: Rect) -> String {
    format!(
        "p{}-{}-{}-{}",
        panel_area.x, panel_area.y, panel_area.width, panel_area.height
    )
}

fn fg(color: Color) -> Style {
    Style::default().fg(color)
}

fn rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0xff, 0xff, 0xff),
    }
}

fn set_char(buf: &mut Buffer, x: u16, y: u16, ch: char, style: Style) {
    let a = buf.area;
    if x < a.x || y < a.y || x >= a.x + a.width || y >= a.y + a.height {
        return; // out-of-bounds — silent no-op (don't panic)
    }
    let cell = &mut buf[(x, y)];
    cell.set_char(ch);
    cell.set_style(style);
}

