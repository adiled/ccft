//! HEAT — horizontal bar chart of activity by time bucket.
//!
//!   y-axis  = time bucket (one row per bucket, chronological top→bottom)
//!   x-axis  = count of requests (bar length proportional to bucket count)
//!
//! Bucketing adapts to the selected range:
//!
//!   sub-36h           → 12 hour-of-day rows at 2h granularity (00, 02, …)
//!   exactly 7d        → 7 day-of-week rows (Mon..Sun, in local week order)
//!   else (incl. all)  → 12 evenly-spaced date rows (M/DD)
//!
//! Bar color encodes average latency in that bucket via `style::heat_color`.

use crate::tui::style;
use crate::tui::App;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let inner = style::panel(f, area, "heat");
    if inner.width < 6 || inner.height == 0 {
        return;
    }

    let buckets = build_buckets(app);
    if buckets.is_empty() {
        return;
    }

    let max_n = buckets.iter().map(|b| b.count).max().unwrap_or(1).max(1);

    // Column widths: label, bar, count.
    let label_w = buckets
        .iter()
        .map(|b| b.label.chars().count())
        .max()
        .unwrap_or(2) as u16;
    let count_w = buckets
        .iter()
        .map(|b| short_count(b.count).chars().count())
        .max()
        .unwrap_or(1) as u16;
    // Reserve label_w + 1 space + count_w + 1 space; the rest is bar.
    let bar_w = inner.width.saturating_sub(label_w + count_w + 2);
    if bar_w < 4 {
        return;
    }

    let max_rows = inner.height as usize;

    let lines: Vec<Line> = buckets
        .into_iter()
        .take(max_rows)
        .map(|b| {
            let label_padded = pad_left(&b.label, label_w);
            let fill_cells = ((b.count as f64 / max_n as f64) * bar_w as f64).round() as u16;
            let fill_cells = fill_cells.min(bar_w);
            let bar = "█".repeat(fill_cells as usize) + &"·".repeat((bar_w - fill_cells) as usize);
            let bar_color = if b.count == 0 {
                style::GREY
            } else {
                style::heat_color(b.avg_lat)
            };
            let count_str = pad_left(&short_count(b.count), count_w);
            Line::from(vec![
                Span::styled(label_padded, style::dim()),
                Span::raw(" "),
                Span::styled(bar, Style::default().fg(bar_color)),
                Span::raw(" "),
                Span::styled(count_str, style::label()),
            ])
        })
        .collect();

    f.render_widget(Paragraph::new(lines), inner);
}

struct Bucket {
    label: String,
    count: u64,
    avg_lat: f64,
}

fn build_buckets(app: &App) -> Vec<Bucket> {
    let local = crate::tui::style::local_offset();

    match app.range_preset {
        // Bounded calendar day(s): hour-of-day at 2h granularity.
        crate::tui::RangePreset::Today | crate::tui::RangePreset::Yesterday => {
            build_hod_buckets_2h(app, local)
        }
        // Rolling sub-day windows + rolling 24h: hour-of-day style buckets
        // ending at the current wall-clock boundary. The 24h builder
        // produces 12 × 2h rows; for tighter windows it'll just show fewer
        // populated rows, which is fine.
        crate::tui::RangePreset::H1
        | crate::tui::RangePreset::H2
        | crate::tui::RangePreset::H4
        | crate::tui::RangePreset::H6
        | crate::tui::RangePreset::H12
        | crate::tui::RangePreset::H24 => build_rolling_24h_buckets(app, local),
        // Multi-day rolling windows: per-day rows, like the 7d builder.
        crate::tui::RangePreset::D3 | crate::tui::RangePreset::Week => {
            build_rolling_7d_buckets(app, local)
        }
        // This calendar week: 7 fixed Mon..Sun rows with future days empty.
        crate::tui::RangePreset::ThisWeek => build_this_week_buckets(app, local),
        // Multi-week / month / all: adaptive based on actual data span.
        crate::tui::RangePreset::W2
        | crate::tui::RangePreset::Mo1
        | crate::tui::RangePreset::All => build_all_buckets(app, local),
    }
}

/// Rolling 24h: 12 chronological 2h-aligned hour buckets ending at the next
/// 2h boundary after now (in local time). Last bar = current 2h slot.
fn build_rolling_24h_buckets(app: &crate::tui::App, local: time::UtcOffset) -> Vec<Bucket> {
    let now = crate::ledger_read::now_secs();
    // Round forward to the next 2h-aligned LOCAL wall-clock boundary.
    let local_offset_secs = local.whole_seconds() as i64;
    let local_now = now as i64 + local_offset_secs;
    let aligned_local = ((local_now / 7200) + 1) * 7200;
    let anchor_ts = (aligned_local - local_offset_secs) as f64;

    (0..12)
        .map(|i| {
            // Bucket i ends at: anchor - (11 - i) * 2h.
            let bucket_end = anchor_ts - (11 - i) as f64 * 2.0 * 3600.0;
            let bucket_start = bucket_end - 2.0 * 3600.0;
            let mut count = 0u64;
            let mut lat_sum = 0u64;
            for r in &app.agg.records {
                if r.ts >= bucket_start && r.ts < bucket_end {
                    count += 1;
                    lat_sum += r.lat;
                }
            }
            let dt = time::OffsetDateTime::from_unix_timestamp(bucket_start as i64)
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
                .to_offset(local);
            let label = format!("{:02}", dt.hour());
            let avg_lat = if count > 0 {
                lat_sum as f64 / count as f64
            } else {
                0.0
            };
            Bucket {
                label,
                count,
                avg_lat,
            }
        })
        .collect()
}

/// All-time: adapt bucketing to actual data span. Always chronological
/// (oldest at top, newest at bottom).
///
///   span < 1.5d   → fall back to rolling-24h shape (12 × 2h)
///   1.5d - 14d    → 1 row per calendar day, up to 14 rows
///   else          → 12 evenly-spaced chronological buckets, year-aware label
fn build_all_buckets(app: &crate::tui::App, local: time::UtcOffset) -> Vec<Bucket> {
    let span = (app.range.until - app.range.since).max(1.0);
    if span < 1.5 * 86400.0 {
        return build_rolling_24h_buckets(app, local);
    }
    if span < 14.5 * 86400.0 {
        return build_chronological_day_buckets(app, local, span);
    }
    build_date_buckets(app, local)
}

/// Chronological day-by-day buckets, up to 14 rows, ending today.
fn build_chronological_day_buckets(
    app: &crate::tui::App,
    local: time::UtcOffset,
    span: f64,
) -> Vec<Bucket> {
    let n = ((span / 86400.0).ceil() as usize).clamp(1, 14);
    let now = crate::ledger_read::now_secs();
    let now_dt = time::OffsetDateTime::from_unix_timestamp(now as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .to_offset(local);
    let today_midnight = now_dt.replace_time(time::Time::MIDNIGHT).unix_timestamp() as f64;

    (0..n)
        .map(|i| {
            let day_start = today_midnight - (n - 1 - i) as f64 * 86400.0;
            let day_end = day_start + 86400.0;
            let mut count = 0u64;
            let mut lat_sum = 0u64;
            for r in &app.agg.records {
                if r.ts >= day_start && r.ts < day_end {
                    count += 1;
                    lat_sum += r.lat;
                }
            }
            let dt = time::OffsetDateTime::from_unix_timestamp(day_start as i64)
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
                .to_offset(local);
            let label = format!("{}/{:02}", u8::from(dt.month()), dt.day());
            let avg_lat = if count > 0 {
                lat_sum as f64 / count as f64
            } else {
                0.0
            };
            Bucket {
                label,
                count,
                avg_lat,
            }
        })
        .collect()
}

/// Rolling 7-day window: 7 calendar-day rows. Bar 0 = 6 days ago,
/// bar 6 = today. Labels are short day-of-week names (Mon, Tue, …).
fn build_rolling_7d_buckets(app: &crate::tui::App, local: time::UtcOffset) -> Vec<Bucket> {
    let now = crate::ledger_read::now_secs();
    let now_dt = time::OffsetDateTime::from_unix_timestamp(now as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .to_offset(local);
    let today_midnight = now_dt.replace_time(time::Time::MIDNIGHT).unix_timestamp() as f64;
    let dow_short = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

    (0..7)
        .map(|i| {
            // i=0 → 6 days ago; i=6 → today.
            let day_start = today_midnight - (6 - i) as f64 * 86400.0;
            let day_end = day_start + 86400.0;
            let mut count = 0u64;
            let mut lat_sum = 0u64;
            for r in &app.agg.records {
                if r.ts >= day_start && r.ts < day_end {
                    count += 1;
                    lat_sum += r.lat;
                }
            }
            let dt = time::OffsetDateTime::from_unix_timestamp(day_start as i64)
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
                .to_offset(local);
            let dow_idx = (dt.weekday().number_from_monday() - 1) as usize;
            let avg_lat = if count > 0 {
                lat_sum as f64 / count as f64
            } else {
                0.0
            };
            Bucket {
                label: dow_short[dow_idx].to_string(),
                count,
                avg_lat,
            }
        })
        .collect()
}

/// This calendar week: 7 fixed Mon..Sun rows. Each bar = that weekday
/// of the week containing `now`. Future days render with count=0.
fn build_this_week_buckets(app: &crate::tui::App, local: time::UtcOffset) -> Vec<Bucket> {
    let now = crate::ledger_read::now_secs();
    let now_dt = time::OffsetDateTime::from_unix_timestamp(now as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .to_offset(local);
    let days_since_monday = (now_dt.weekday().number_from_monday() - 1) as i64;
    let monday_dt = now_dt - time::Duration::days(days_since_monday);
    let monday_midnight_ts = monday_dt
        .replace_time(time::Time::MIDNIGHT)
        .unix_timestamp() as f64;

    let labels = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    (0..7)
        .map(|i| {
            let day_start = monday_midnight_ts + i as f64 * 86400.0;
            let day_end = day_start + 86400.0;
            let mut count = 0u64;
            let mut lat_sum = 0u64;
            for r in &app.agg.records {
                if r.ts >= day_start && r.ts < day_end {
                    count += 1;
                    lat_sum += r.lat;
                }
            }
            let avg_lat = if count > 0 {
                lat_sum as f64 / count as f64
            } else {
                0.0
            };
            Bucket {
                label: labels[i].to_string(),
                count,
                avg_lat,
            }
        })
        .collect()
}

/// Sub-36h ranges: 12 rows at 2-hour granularity (00, 02, 04, ..., 22).
/// Each row aggregates two consecutive hours of the day.
fn build_hod_buckets_2h(app: &crate::tui::App, local: time::UtcOffset) -> Vec<Bucket> {
    let mut counts = vec![0u64; 12];
    let mut lat_sums = vec![0u64; 12];
    for r in &app.agg.records {
        let dt = time::OffsetDateTime::from_unix_timestamp(r.ts as i64)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
            .to_offset(local);
        let idx = (dt.hour() / 2) as usize;
        if idx < 12 {
            counts[idx] += 1;
            lat_sums[idx] += r.lat;
        }
    }
    (0..12)
        .map(|i| {
            let n = counts[i];
            let avg_lat = if n > 0 {
                lat_sums[i] as f64 / n as f64
            } else {
                0.0
            };
            Bucket {
                label: format!("{:02}", i * 2),
                count: n,
                avg_lat,
            }
        })
        .collect()
}

/// Long ranges (>14d, evenly-spaced 12 buckets). Labels switch to month/year
/// format when the range crosses a year boundary so that e.g. May 2025 and
/// May 2026 don't both label as "5/04" looking unsorted.
fn build_date_buckets(app: &crate::tui::App, local: time::UtcOffset) -> Vec<Bucket> {
    const N: usize = 12;
    let span = (app.range.until - app.range.since).max(1.0);
    let bucket_s = span / N as f64;

    // Year-aware label format: if the range crosses a calendar year, use
    // M/YY so chronological order stays visually obvious.
    let since_year = year_of(app.range.since, local);
    let until_year = year_of(app.range.until, local);
    let multi_year = until_year != since_year;

    let mut counts = [0u64; N];
    let mut lat_sums = [0u64; N];
    for r in &app.agg.records {
        let idx = ((r.ts - app.range.since) / bucket_s).floor() as i64;
        if idx >= 0 && (idx as usize) < N {
            counts[idx as usize] += 1;
            lat_sums[idx as usize] += r.lat;
        }
    }
    (0..N)
        .map(|i| {
            let mid_ts = app.range.since + bucket_s * (i as f64 + 0.5);
            let dt = time::OffsetDateTime::from_unix_timestamp(mid_ts as i64)
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
                .to_offset(local);
            let label = if multi_year {
                format!("{}/{:02}", u8::from(dt.month()), dt.year() % 100)
            } else {
                format!("{}/{:02}", u8::from(dt.month()), dt.day())
            };
            let n = counts[i];
            let avg_lat = if n > 0 {
                lat_sums[i] as f64 / n as f64
            } else {
                0.0
            };
            Bucket {
                label,
                count: n,
                avg_lat,
            }
        })
        .collect()
}

fn year_of(epoch: f64, local: time::UtcOffset) -> i32 {
    time::OffsetDateTime::from_unix_timestamp(epoch as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .to_offset(local)
        .year()
}

fn short_count(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn pad_left(s: &str, w: u16) -> String {
    let cur = s.chars().count() as u16;
    if cur >= w {
        s.to_string()
    } else {
        format!("{}{}", " ".repeat((w - cur) as usize), s)
    }
}
