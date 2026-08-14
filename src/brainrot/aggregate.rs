//! Record aggregation + bot/driver scoring (V·L·P·V signal model).
//! Ports `aggregate()`, `bot_score`, `driver_score`, and the `_*` helpers
//! from cc-flytrap/brainrot.py. Math is identical (no content inspection,
//! only behaviour from the ledger telemetry).

use crate::ledger_read::Record;
use std::collections::{HashMap, HashSet};

#[derive(Default, Debug)]
pub struct Aggregate {
    pub n: u64,
    pub r#in: u64,
    pub out: u64,
    pub tot: u64,
    pub lat_sum: u64,
    pub lat_max: u64,
    pub lats: Vec<u64>,
    pub first_ts: Option<f64>,
    pub last_ts: Option<f64>,
    pub models: HashMap<String, u64>,
    pub sessions: HashSet<String>,
    pub by_hour: HashMap<u8, HourBucket>,
    pub by_minute: HashMap<i64, MinuteBucket>,
    pub records: Vec<Record>,
}

#[derive(Default, Debug)]
pub struct HourBucket {
    pub n: u64,
    pub tot: u64,
    pub lat_sum: u64,
}

#[derive(Default, Debug)]
pub struct MinuteBucket {
    pub n: u64,
    pub tot: u64,
}

impl Aggregate {
    pub fn ingest<I: IntoIterator<Item = Record>>(records: I) -> Self {
        let mut a = Aggregate::default();
        for r in records {
            a.n += 1;
            a.r#in += r.r#in;
            a.out += r.out;
            a.tot += r.tot;
            a.lat_sum += r.lat;
            if r.lat > a.lat_max { a.lat_max = r.lat; }
            a.lats.push(r.lat);

            let ts = r.ts;
            a.first_ts = Some(a.first_ts.map_or(ts, |x| x.min(ts)));
            a.last_ts = Some(a.last_ts.map_or(ts, |x| x.max(ts)));

            let model = r.model.clone().unwrap_or_else(|| "unknown".into());
            *a.models.entry(model).or_insert(0) += 1;
            if let Some(s) = &r.sid {
                a.sessions.insert(s.clone());
            }

            let hour = ((ts as i64).rem_euclid(86400) / 3600) as u8;
            let hb = a.by_hour.entry(hour).or_default();
            hb.n += 1;
            hb.tot += r.tot;
            hb.lat_sum += r.lat;

            let minute = (ts as i64) / 60;
            let mb = a.by_minute.entry(minute).or_default();
            mb.n += 1;
            mb.tot += r.tot;

            a.records.push(r);
        }
        a
    }

    fn gaps(&self) -> Vec<f64> {
        let mut ts: Vec<f64> = self.records.iter().map(|r| r.ts).collect();
        ts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        ts.windows(2).map(|w| w[1] - w[0]).collect()
    }
}

/// Driver-vs-bot turn classification.
///
/// `Driver` = first request of a session OR any request following a gap >
/// `BOT_LOOP_THRESHOLD` seconds (5s by default). `Bot` = anything else
/// (continuation of a tool-loop).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TurnKind {
    Driver,
    Bot,
}

pub const BOT_LOOP_THRESHOLD: f64 = 5.0;

/// Classify each record as Driver or Bot. Returns a vec aligned 1:1 with
/// the input slice. Stable: walks each session in chronological order and
/// inspects the inter-arrival gap from the previous response end.
pub fn classify_turns(records: &[Record]) -> Vec<TurnKind> {
    let mut kinds = vec![TurnKind::Driver; records.len()];
    let mut by_sid: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in records.iter().enumerate() {
        let sid = r.sid.clone().unwrap_or_else(|| "_orphan".into());
        by_sid.entry(sid).or_default().push(i);
    }
    for (_sid, mut idxs) in by_sid {
        idxs.sort_by(|a, b| {
            records[*a]
                .ts
                .partial_cmp(&records[*b].ts)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut prev_te: Option<f64> = None;
        for i in &idxs {
            let r = &records[*i];
            let te = if r.te > 0.0 { r.te } else { r.ts };
            kinds[*i] = match prev_te {
                None => TurnKind::Driver,
                Some(prev) if (r.ts - prev) > BOT_LOOP_THRESHOLD => TurnKind::Driver,
                Some(_) => TurnKind::Bot,
            };
            prev_te = Some(te);
        }
    }
    kinds
}

// ─── Probabilistic turn classification ──────────────────────────────────────
//
// `classify_turns` above uses a hardcoded `BOT_LOOP_THRESHOLD` (5s): any
// in-session gap > 5s is a driver turn, else a bot continuation. That magic
// constant is brittle — it can't adapt to a user whose tool loops are
// consistently 8s (a slow tool), nor to a fast typist whose think-gaps are
// 3s. The probabilistic classifier fits a 2-component log-normal mixture to
// the pooled inter-arrival gaps and classifies each turn by *posterior
// probability*, replacing the fixed 5s cut with a learned decision boundary.
//
// It is a strict drop-in for `classify_turns` (same input, same `Vec<TurnKind>`
// output, same first-in-session + `_orphan` semantics) and is fully
// self-contained: it needs no extra state and no dependency on the
// regime-change / time-series work. When the fit is unstable it falls back
// to the deterministic classifier, so it's safe to swap in anywhere.

/// Half of ln(2π) = 0.9189…, the normalizer of the log-normal pdf.
const LN_2PI_HALF: f64 = 0.9189385332046727;
/// A component must own at least this many gaps or the fit is refused.
/// Kept low (2) so the model works on small windows / thin driver clusters;
/// EM + MIN_PRIOR + the deterministic fallback are the real degeneracy guards.
const MIN_GAPS_PER_COMPONENT: usize = 2;
/// A fitted prior below this is treated as component collapse → fallback.
const MIN_PRIOR: f64 = 0.02;
/// Log-normal sigma floor: prevents a component from collapsing to a point.
const SIGMA_FLOOR: f64 = 0.1;
const EM_MAX_ITERS: u32 = 120;
const EM_TOL: f64 = 1e-7;

/// Minimum counted user-text chars before the wordology (lexical) axis is
/// trusted. Short prompts have inflated type-token ratio and low n-gram
/// entropy regardless of author, so the lexical signal is skipped below this.
const MIN_LEX_CHARS: u64 = 40;

/// Fitted 2-component log-normal mixture over pooled inter-arrival gaps.
/// "bot" is always the short-gap component (tool-loop continuation), "drv"
/// the long-gap component (human think/type time). `threshold_sec` is the
/// learned decision boundary — the gap where the posterior of "bot" crosses
/// 0.5 — which replaces the hardcoded `BOT_LOOP_THRESHOLD`.
#[derive(Clone, Debug)]
pub struct GapMixture {
    pub pi_bot: f64,
    pub mu_bot: f64,    // ln-median of bot gaps
    pub sigma_bot: f64, // log-spread of bot gaps
    pub pi_drv: f64,
    pub mu_drv: f64,
    pub sigma_drv: f64,
    pub n_gaps: usize,
    pub threshold_sec: f64,
    pub log_lik: f64,
    pub iterations: u32,
}

/// Pooled inter-arrival gaps across all sessions: for each record after the
/// first in a session, elapsed seconds from the previous response end.
/// Mirrors the gap logic in `classify_turns` exactly so the probabilistic
/// classifier sees the same data the deterministic one does.
fn pooled_gaps(records: &[Record]) -> Vec<f64> {
    let mut gaps: Vec<f64> = Vec::new();
    let mut by_sid: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in records.iter().enumerate() {
        let sid = r.sid.clone().unwrap_or_else(|| "_orphan".into());
        by_sid.entry(sid).or_default().push(i);
    }
    for (_sid, mut idxs) in by_sid {
        idxs.sort_by(|a, b| {
            records[*a]
                .ts
                .partial_cmp(&records[*b].ts)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut prev_te: Option<f64> = None;
        for i in &idxs {
            let r = &records[*i];
            let te = if r.te > 0.0 { r.te } else { r.ts };
            if let Some(prev) = prev_te {
                let g = r.ts - prev;
                if g > 1e-6 {
                    gaps.push(g);
                }
            }
            prev_te = Some(te);
        }
    }
    gaps
}

/// (ln-mean, ln-std) of a positive sample.
fn ln_moments(xs: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let mu = xs.iter().map(|x| x.ln()).sum::<f64>() / n;
    let var = xs.iter().map(|x| (x.ln() - mu).powi(2)).sum::<f64>() / n;
    (mu, var.max(0.0).sqrt())
}

fn ln_pdf_lnorm(g: f64, mu: f64, sigma: f64) -> f64 {
    let z = (g.ln() - mu) / sigma;
    -(z * z) / 2.0 - g.ln() - sigma.ln() - LN_2PI_HALF
}

fn posterior_bot(g: f64, mu_b: f64, sg_b: f64, pi_b: f64, mu_d: f64, sg_d: f64, pi_d: f64) -> f64 {
    let l_b = ln_pdf_lnorm(g, mu_b, sg_b) + pi_b.ln();
    let l_d = ln_pdf_lnorm(g, mu_d, sg_d) + pi_d.ln();
    let m = l_b.max(l_d);
    let e_b = (l_b - m).exp();
    let e_d = (l_d - m).exp();
    e_b / (e_b + e_d)
}

/// Where does posterior("bot") cross 0.5? Binary search between the two
/// component medians (posterior is monotone decreasing across that band).
/// Falls back to the geometric midpoint when there's no clean crossing.
fn crossover_gap(mu_b: f64, sg_b: f64, pi_b: f64, mu_d: f64, sg_d: f64, pi_d: f64) -> f64 {
    let lo = mu_b.min(mu_d).exp();
    let hi = mu_b.max(mu_d).exp();
    let p_lo = posterior_bot(lo, mu_b, sg_b, pi_b, mu_d, sg_d, pi_d);
    let p_hi = posterior_bot(hi, mu_b, sg_b, pi_b, mu_d, sg_d, pi_d);
    if p_lo <= 0.5 || p_hi >= 0.5 {
        return (lo * hi).sqrt();
    }
    let mut lo = lo;
    let mut hi = hi;
    for _ in 0..60 {
        let mid = (lo + hi) / 2.0;
        if posterior_bot(mid, mu_b, sg_b, pi_b, mu_d, sg_d, pi_d) > 0.5 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) / 2.0
}

/// Fit a 2-component log-normal mixture to pooled gaps by EM, warm-started
/// from the deterministic 5s threshold when it can see both clusters,
/// else a median split (handles "slow tool" sessions where every loop gap
/// exceeds 5s). Returns None when the data can't support a stable split —
/// callers fall back to `classify_turns`.
pub fn fit_gap_mixture(records: &[Record]) -> Option<GapMixture> {
    let gaps = pooled_gaps(records);
    let n = gaps.len();
    if n < MIN_GAPS_PER_COMPONENT * 2 {
        return None;
    }

    let split = |cut: f64| {
        (
            gaps.iter().copied().filter(|g| *g <= cut).collect::<Vec<f64>>(),
            gaps.iter().copied().filter(|g| *g > cut).collect::<Vec<f64>>(),
        )
    };
    let (bot, drv) = {
        let (b, d) = split(BOT_LOOP_THRESHOLD);
        if b.len() >= MIN_GAPS_PER_COMPONENT && d.len() >= MIN_GAPS_PER_COMPONENT {
            (b, d)
        } else {
            let mut sorted = gaps.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            split(sorted[sorted.len() / 2])
        }
    };
    if bot.len() < MIN_GAPS_PER_COMPONENT || drv.len() < MIN_GAPS_PER_COMPONENT {
        return None;
    }

    let (m_b, s_b) = ln_moments(&bot);
    let (m_d, s_d) = ln_moments(&drv);
    let mut pi_b = bot.len() as f64 / n as f64;
    let mut pi_d = 1.0 - pi_b;
    let mut mu_b = m_b;
    let mut sigma_b = s_b.max(SIGMA_FLOOR);
    let mut mu_d = m_d;
    let mut sigma_d = s_d.max(SIGMA_FLOOR);

    let mut ll = f64::NEG_INFINITY;
    let mut iters = 0u32;
    for _ in 0..EM_MAX_ITERS {
        iters += 1;
        // E-step
        let mut s_b = 0.0;
        let mut s_d = 0.0;
        let mut s_b_ln = 0.0;
        let mut s_d_ln = 0.0;
        let mut s_b_l2 = 0.0;
        let mut s_d_l2 = 0.0;
        let mut new_ll = 0.0;
        for g in &gaps {
            let l_b = ln_pdf_lnorm(*g, mu_b, sigma_b) + pi_b.ln();
            let l_d = ln_pdf_lnorm(*g, mu_d, sigma_d) + pi_d.ln();
            let m = l_b.max(l_d);
            let e_b = (l_b - m).exp();
            let e_d = (l_d - m).exp();
            let denom = e_b + e_d;
            let gb = e_b / denom;
            let gd = e_d / denom;
            s_b += gb;
            s_d += gd;
            let lng = g.ln();
            s_b_ln += gb * lng;
            s_d_ln += gd * lng;
            s_b_l2 += gb * lng * lng;
            s_d_l2 += gd * lng * lng;
            new_ll += m + denom.ln();
        }
        if s_b < 1e-9 || s_d < 1e-9 {
            return None; // one component owns every gap
        }
        // M-step
        pi_b = s_b / n as f64;
        pi_d = s_d / n as f64;
        if pi_b < MIN_PRIOR || pi_d < MIN_PRIOR {
            return None;
        }
        mu_b = s_b_ln / s_b;
        mu_d = s_d_ln / s_d;
        sigma_b = (s_b_l2 / s_b - mu_b * mu_b).max(0.0).sqrt().max(SIGMA_FLOOR);
        sigma_d = (s_d_l2 / s_d - mu_d * mu_d).max(0.0).sqrt().max(SIGMA_FLOOR);
        let dll = new_ll - ll;
        ll = new_ll;
        if dll.abs() < EM_TOL * (1.0 + ll.abs()) {
            break;
        }
    }

    // Keep "bot" = short-gap component (warm start should preserve order,
    // but guard against a swapped convergence).
    if mu_b > mu_d {
        std::mem::swap(&mut pi_b, &mut pi_d);
        std::mem::swap(&mut mu_b, &mut mu_d);
        std::mem::swap(&mut sigma_b, &mut sigma_d);
    }

    let threshold_sec = crossover_gap(mu_b, sigma_b, pi_b, mu_d, sigma_d, pi_d);
    Some(GapMixture {
        pi_bot: pi_b,
        mu_bot: mu_b,
        sigma_bot: sigma_b,
        pi_drv: pi_d,
        mu_drv: mu_d,
        sigma_drv: sigma_d,
        n_gaps: n,
        threshold_sec,
        log_lik: ll,
        iterations: iters,
    })
}

/// Session-level momentum context for the wordology axis: evidence that
/// accumulates across a session's turns, not per-message.
#[derive(Clone, Copy, Default)]
struct MomCtx {
    /// Coefficient of variation of plain-text size across this session's
    /// turns (burstiness). Low = uniform, template-sized prompts ⇒ a bot
    /// driving; high = bursty, human-sized typing. `-1.0` = not enough turns
    /// to measure (no signal).
    cv_size: f64,
}

/// How machine-like is this turn's *prompt text*? A 0..1 likelihood that
/// the plain-text user message was authored by a bot rather than a human:
///   * `tr_ch > 0`   — a tool_result continuation is bot feedback by
///                     construction → certain bot (1.0).
///   * `u_ch` too low — no text to analyze → no signal (None).
///   * `lex_div == 0` — older-schema record → no lexical signal (None).
///   * otherwise, the wordology features — low lexical diversity (repetition),
///     low bigram entropy (repetition), low function-word fraction → machine.
///
/// This is the *second axis* that gap timing can't see: a bot controlling
/// the prompting writes plain text that paces like a human (so gap says
/// "driver") but is machine-flat in the words. It's a heuristic prior
/// grounded in stylometry's direction of effects (repetition ⇒ machine),
/// NOT a fitted model — the ledger has no labels to train on.
///
/// Momentum (cross-turn) terms fold in `mom` + the record's `nvt`:
///   * `nvt` high — the same content bigrams keep coming back turn after
///     turn (template reuse ⇒ a bot driving the prompting). Captured at the
///     proxy against an in-memory per-session set; only the fraction persists.
///   * `cv_size` low — prompt sizes are uniform (template-sized ⇒ machine);
///     high/absent means human burstiness → no signal.
fn machine_likeness(r: &Record, mom: &MomCtx) -> Option<f64> {
    if r.tr_ch > 0 {
        return Some(1.0);
    }
    if r.u_ch < MIN_LEX_CHARS || r.lex_div <= 0.0 {
        return None;
    }
    // n-gram entropy scale: bigrams on ≥40 chars of prose sit roughly 2–6 bits.
    let p_nge = 1.0 - (r.ngram_entropy / 4.0).clamp(0.0, 1.0);
    // lexical diversity: low TTR ⇒ repetitive ⇒ machine.
    let p_ttr = 1.0 - r.lex_div.clamp(0.0, 1.0);
    // function-word fraction: humans lean on them; models are content-dense.
    let p_fnx = 1.0 - (r.fn_word_frac / 0.5).clamp(0.0, 1.0);
    let mut p = 0.5 * p_ttr + 0.35 * p_nge + 0.15 * p_fnx;
    // Momentum terms only REINFORCE a machine-leaning read — they never
    // override a clearly human one. Both are gated behind p > 0.5.
    if p > 0.5 {
        // Momentum 1 — cross-turn template reuse. A near-total repeat (nvt
        // ~0.9) is the smoking gun; moderate reuse just nudges the read.
        if r.nvt > 0.0 {
            p = 0.5 * p + 0.5 * r.nvt.clamp(0.0, 1.0);
        }
        // Momentum 2 — session burstiness. Uniform prompt sizes (cv≈0) ⇒ a
        // bot emitting template-sized prompts; bursty human typing ⇒ no nudge.
        if mom.cv_size >= 0.0 {
            let p_burst = 1.0 - (mom.cv_size / 0.5).clamp(0.0, 1.0);
            p = 0.7 * p + 0.3 * p_burst;
        }
    }
    Some(p.clamp(0.0, 1.0))
}

/// Probabilistic turn classifier — drop-in for `classify_turns`. Fits a
/// 2-component log-normal mixture over pooled gaps and labels each turn by
/// posterior probability (short-gap/bot posterior > 0.5 → Bot), then fuses
/// the wordology axis (`machine_likeness`) as a second, content-free signal
/// that corrects the pacing proxy where it's confident: machine-flat text
/// forces Bot even when the gap was long (the "another bot driving" case);
/// clearly human text forces Driver even when the gap was short (a fast
/// typist). Falls back to the deterministic 5s classifier when the fit is
/// unstable, so it is safe to substitute everywhere `classify_turns` is used.
pub fn classify_turns_prob(records: &[Record]) -> Vec<TurnKind> {
    classify_turns_prob_with_model(records).0
}

/// As above but also returns the fitted model (`None` when it fell back to
/// the deterministic threshold). Lets callers surface "your learned bot-loop
/// gap is ~Xs, your human turn gap is ~Ys, decision boundary ~Zs" and is
/// what the tests assert against.
pub fn classify_turns_prob_with_model(records: &[Record]) -> (Vec<TurnKind>, Option<GapMixture>) {
    let mut kinds = vec![TurnKind::Driver; records.len()];
    if records.is_empty() {
        return (kinds, None);
    }
    let mix = fit_gap_mixture(records);
    let mut by_sid: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in records.iter().enumerate() {
        let sid = r.sid.clone().unwrap_or_else(|| "_orphan".into());
        by_sid.entry(sid).or_default().push(i);
    }
    for (_sid, mut idxs) in by_sid {
        idxs.sort_by(|a, b| {
            records[*a]
                .ts
                .partial_cmp(&records[*b].ts)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // Momentum: this session's burstiness (CV of plain-text size).
        // Uniform template-sized prompts ⇒ machine; bursty ⇒ human. -1 = no
        // measurement (fewer than two plain-text turns).
        let sizes: Vec<f64> = idxs
            .iter()
            .filter(|i| records[**i].u_ch > 0)
            .map(|i| records[*i].u_ch as f64)
            .collect();
        let cv_size = if sizes.len() >= 2 {
            let mean = sizes.iter().sum::<f64>() / sizes.len() as f64;
            if mean > 0.0 {
                let var = sizes
                    .iter()
                    .map(|s| (s - mean) * (s - mean))
                    .sum::<f64>()
                    / sizes.len() as f64;
                var.sqrt() / mean
            } else {
                -1.0
            }
        } else {
            -1.0
        };
        let mom = MomCtx { cv_size };
        let mut prev_te: Option<f64> = None;
        for i in &idxs {
            let r = &records[*i];
            let te = if r.te > 0.0 { r.te } else { r.ts };
            // Pacing prior: the gap-mixture posterior when the fit succeeded,
            // else the deterministic 5s rule (a hard 0/1 pacing prior).
            let p_gap = match prev_te {
                None => 0.0,
                Some(prev) => match &mix {
                    Some(m) => posterior_bot(
                        r.ts - prev,
                        m.mu_bot,
                        m.sigma_bot,
                        m.pi_bot,
                        m.mu_drv,
                        m.sigma_drv,
                        m.pi_drv,
                    ),
                    None => {
                        if r.ts - prev > BOT_LOOP_THRESHOLD {
                            0.0
                        } else {
                            1.0
                        }
                    }
                },
            };
            let mut p = p_gap;
            // Wordology axis: correct the pacing proxy where it's confident.
            // tr_ch>0 or machine-flat text ⇒ bot even on a long gap ("another
            // bot driving the prompting"); clearly human text ⇒ driver even on
            // a fast gap (a fast typist). Runs on top of *either* prior, so it
            // also rescues the all-slow session where no gap mixture fits.
            if let Some(p_lex) = machine_likeness(r, &mom) {
                if p_lex >= 0.65 {
                    // machine-flat text (or tr_ch>0) ⇒ bot even on a long gap
                    p = p.max(0.75);
                } else if p_lex <= 0.25 {
                    // clearly human text ⇒ driver even on a fast gap
                    p = p.min(0.25);
                } else {
                    p = 0.6 * p + 0.4 * p_lex;
                }
            }
            kinds[*i] = if p > 0.5 { TurnKind::Bot } else { TurnKind::Driver };
            prev_te = Some(te);
        }
    }
    (kinds, mix)
}

fn quantile(xs: &mut [f64], q: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let k = (xs.len() - 1) as f64 * q;
    let f = k.floor() as usize;
    let c = (f + 1).min(xs.len() - 1);
    xs[f] + (xs[c] - xs[f]) * (k - f as f64)
}

/// Ordinary-least-squares slope of y on x. 0 when degenerate. Kept for
/// future regime-change detection on inter-arrival gaps; not currently
/// wired into any score, but cheap to retain.
#[allow(dead_code)]
fn slope(pairs: &[(f64, f64)]) -> f64 {
    let n = pairs.len();
    if n < 3 {
        return 0.0;
    }
    let nf = n as f64;
    let sx: f64 = pairs.iter().map(|(x, _)| x).sum();
    let sy: f64 = pairs.iter().map(|(_, y)| y).sum();
    let sxx: f64 = pairs.iter().map(|(x, _)| x * x).sum();
    let sxy: f64 = pairs.iter().map(|(x, y)| x * y).sum();
    let denom = nf * sxx - sx * sx;
    if denom == 0.0 {
        0.0
    } else {
        (nf * sxy - sx * sy) / denom
    }
}

// ─── Robust statistics ───────────────────────────────────────────────────────

fn median(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut s = xs.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    if n.is_multiple_of(2) {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    } else {
        s[n / 2]
    }
}

/// Median Absolute Deviation, scaled to be comparable to stdev for a normal
/// distribution (×1.4826). Robust to outliers — a single 100-second tool loop
/// doesn't distort the dispersion estimate the way stdev would.
fn mad(xs: &[f64], med: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let dev: Vec<f64> = xs.iter().map(|x| (x - med).abs()).collect();
    median(&dev) * 1.4826
}

/// Coefficient of variation = stdev / |mean|. Returns 0 for degenerate input.
fn cv(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
    let stdev = var.sqrt();
    if mean.abs() < 1e-9 {
        0.0
    } else {
        stdev / mean
    }
}

/// Robust z-score: `(x - median) / MAD`. Returns 0 when MAD is degenerate
/// (no dispersion in the baseline) so we don't over-claim signal.
fn robust_z(x: f64, med: f64, mad: f64) -> f64 {
    if mad <= 1e-9 {
        return 0.0;
    }
    (x - med) / mad
}

/// Map a robust z-score to a 0..100 distress score:
///   z = 0          →  50  (at baseline / typical)
///   z = +scale     →  ~88 (notably worse than usual)
///   z = -scale     →  ~12 (notably better than usual)
///   z → ±∞         →  100 / 0 (saturates gracefully)
///
/// Convention: positive z means "more concerning" — each component flips its
/// sign to ensure that holds (e.g., bot brevity uses `baseline - current` so
/// shorter-than-usual output produces positive z).
fn logistic_score(z: f64, scale: f64) -> f64 {
    50.0 + 50.0 * (z / scale).tanh()
}

/// Per-record cap on `u_ch`. Anything beyond this is treated as the cap —
/// genuine user typing per single API request never exceeds a few thousand
/// chars; values in the tens-of-thousands are auto-injected content (large
/// CLAUDE.md, slash-command expansion, IDE context, etc.) we haven't
/// strip-classified yet. Defense in depth alongside the strip rules in
/// handler.rs::count_user_text.
const RECORD_U_CH_CAP: u64 = 5000;

/// Winsorize a slice in place: clip every value outside the [p_low, p_high]
/// percentile band to the band edges. p_low / p_high in 0.0..=1.0. Returns
/// the band edges actually used.
fn winsorize(xs: &mut [f64], p_low: f64, p_high: f64) -> (f64, f64) {
    if xs.len() < 4 {
        return (xs.iter().cloned().fold(f64::INFINITY, f64::min),
                xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max));
    }
    let mut sorted: Vec<f64> = xs.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let lo_idx = ((sorted.len() as f64 * p_low) as usize).min(sorted.len() - 1);
    let hi_idx = ((sorted.len() as f64 * p_high) as usize).min(sorted.len() - 1);
    let lo = sorted[lo_idx];
    let hi = sorted[hi_idx];
    for v in xs.iter_mut() {
        *v = v.clamp(lo, hi);
    }
    (lo, hi)
}

/// Sample mean. Returns 0 on empty input.
fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() { return 0.0; }
    xs.iter().sum::<f64>() / xs.len() as f64
}

/// Sample standard deviation (n-1 denominator). Returns 0 when n<2.
fn stddev(xs: &[f64], m: f64) -> f64 {
    if xs.len() < 2 { return 0.0; }
    let var = xs.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (xs.len() - 1) as f64;
    var.sqrt()
}

// ─── Baseline: the user's historical fingerprint ─────────────────────────────
//
// Computed once from the full ledger (or whatever set of records the caller
// provides). Subsequent score computations on a window are z-scored against
// this fingerprint. So "high score" means "this window is unusual for YOU,"
// not "this window crosses some absolute threshold guessed at design time."

#[derive(Default, Debug, Clone)]
pub struct Baseline {
    pub n_records: u64,
    pub n_sessions: usize,

    // Per-record metric distributions
    pub out_med: f64,
    pub out_mad: f64,
    pub in_med: f64,
    pub in_mad: f64,
    pub ms_per_token_med: f64,
    pub ms_per_token_mad: f64,

    // Cache miss rate (single scalar)
    pub cache_miss_rate: f64,

    // Per-session statistic distributions
    pub session_out_cv_med: f64,
    pub session_out_cv_mad: f64,
    pub session_models_med: f64,
    pub session_models_mad: f64,
    pub gap_cv_med: f64,
    pub gap_cv_mad: f64,

    // Window-rate scalar (sessions/hour over the entire baseline span)
    pub sessions_per_hour: f64,

    // Driver kinetics: user-typed chars per minute. Computed as winsorized
    // mean + winsorized std-dev across per-day rates so a single outlier
    // day (e.g., one massive paste, one degenerate auto-injected block we
    // failed to strip) can't poison the comparison anchor or the spread.
    // u_ch values are also clamped per-record at RECORD_U_CH_CAP before
    // aggregation (defense in depth — a 250k-char "user message" is
    // structurally not user input).
    pub user_chars_per_min_mean: f64,
    pub user_chars_per_min_std: f64,
    /// Kept for back-compat / debug-scores display; same data, robust
    /// statistics for the same per-day distribution.
    pub user_chars_per_min_med: f64,
    pub user_chars_per_min_mad: f64,
    pub n_records_with_u_ch: u64,

    // Signal metrics — distributions of cc-based ratios across days.
    // Used by `compute_signal` to z-score the current window and surface
    // a phrase describing the dominant deviation.
    pub investigation_med: f64, // cc / out per day
    pub investigation_mad: f64,
    pub amplification_med: f64, // cc / u_ch per day (when u_ch > 0)
    pub amplification_mad: f64,
    pub throughput_med: f64,    // cc / active_minutes per day
    pub throughput_mad: f64,

    // Latency-tier percentiles (for dynamic word labels). Computed from
    // baseline ms_per_token distribution so each user gets thresholds
    // calibrated to their own normal.
    pub lat_p20: f64,
    pub lat_p40: f64,
    pub lat_p60: f64,
    pub lat_p80: f64,
}

impl Baseline {
    /// Empty baseline — used when no historical data exists yet (brand-new
    /// install). Score functions interpret this as "no signal" and return 0.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build a baseline fingerprint from an arbitrary record set. Typically
    /// called with the entire ledger so subsequent windowed scores express
    /// "deviation from your typical behavior."
    pub fn from_records(records: &[Record]) -> Self {
        if records.is_empty() {
            return Self::default();
        }

        // Per-record arrays
        let outs: Vec<f64> = records.iter().map(|r| r.out as f64).collect();
        let ins: Vec<f64> = records.iter().map(|r| r.r#in as f64).collect();
        let ms_per_token: Vec<f64> = records
            .iter()
            .filter(|r| r.out > 0)
            .map(|r| r.lat as f64 / r.out as f64)
            .collect();

        let out_med = median(&outs);
        let out_mad = mad(&outs, out_med);
        let in_med = median(&ins);
        let in_mad = mad(&ins, in_med);
        let ms_per_token_med = median(&ms_per_token);
        let ms_per_token_mad = mad(&ms_per_token, ms_per_token_med);

        // Cache miss rate: cc / (cc + cr) globally. Single scalar — score
        // functions use a synthetic MAD around it for z-scoring.
        let total_cr: u64 = records.iter().map(|r| r.cr).sum();
        let total_cc: u64 = records.iter().map(|r| r.cc).sum();
        let cache_miss_rate = if total_cr + total_cc > 0 {
            total_cc as f64 / (total_cr + total_cc) as f64
        } else {
            0.0
        };

        // Per-session metrics
        let by_sid = group_records_by_sid(records);
        let n_sessions = by_sid.len();

        let mut session_out_cvs: Vec<f64> = Vec::new();
        let mut session_models: Vec<f64> = Vec::new();
        let mut session_gap_cvs: Vec<f64> = Vec::new();

        for recs in by_sid.values() {
            // Output-size CV within session (bot wandering)
            let outs: Vec<f64> = recs.iter().map(|r| r.out as f64).collect();
            session_out_cvs.push(cv(&outs));

            // Unique models within session (driver thrash)
            let mut models: HashSet<String> = HashSet::new();
            for r in recs {
                if let Some(m) = &r.model {
                    models.insert(m.clone());
                }
            }
            session_models.push(models.len() as f64);

            // Inter-arrival gap CV within session (driver pace volatility)
            let mut sorted = recs.clone();
            sorted.sort_by(|a, b| {
                a.ts.partial_cmp(&b.ts).unwrap_or(std::cmp::Ordering::Equal)
            });
            if sorted.len() >= 3 {
                let gaps: Vec<f64> =
                    sorted.windows(2).map(|w| w[1].ts - w[0].ts).collect();
                session_gap_cvs.push(cv(&gaps));
            }
        }

        let session_out_cv_med = median(&session_out_cvs);
        let session_out_cv_mad = mad(&session_out_cvs, session_out_cv_med);
        let session_models_med = median(&session_models);
        let session_models_mad = mad(&session_models, session_models_med);
        let gap_cv_med = median(&session_gap_cvs);
        let gap_cv_mad = mad(&session_gap_cvs, gap_cv_med);

        // Sessions per hour over the full baseline span
        let first_ts = records.iter().map(|r| r.ts).fold(f64::INFINITY, f64::min);
        let last_ts = records
            .iter()
            .map(|r| r.ts)
            .fold(f64::NEG_INFINITY, f64::max);
        let span_hours = ((last_ts - first_ts) / 3600.0).max(1.0 / 60.0);
        let sessions_per_hour = n_sessions as f64 / span_hours;

        // Driver kinetics: per-day sustained rate. Group records by local
        // date, compute each day's total u_ch / active_minutes_that_day
        // (active span = first-to-last record on that date). Take median +
        // MAD across days. This matches the per-window driver_chars_per_min
        // metric below, so the score's z-test compares apples-to-apples.
        //
        // Earlier attempt computed per-record bursts (a 500-char message
        // after a 30s gap = 1000 chars/min); MAD across those was ~170,
        // dwarfing any real day-to-day variation. Per-day rates land in a
        // 50-200 chars/min band → MAD ~30-50 → z values actually move.
        let local_offset = time::UtcOffset::current_local_offset()
            .unwrap_or(time::UtcOffset::UTC);
        let mut by_date: HashMap<(i32, u8, u8), Vec<&Record>> = HashMap::new();
        for r in records {
            if r.u_ch == 0 { continue; }
            let dt = time::OffsetDateTime::from_unix_timestamp(r.ts as i64)
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
                .to_offset(local_offset);
            by_date
                .entry((dt.year(), u8::from(dt.month()), dt.day()))
                .or_default()
                .push(r);
        }
        let mut daily_rates: Vec<f64> = Vec::new();
        let mut n_records_with_u_ch = 0u64;
        for (_date, recs) in &by_date {
            n_records_with_u_ch += recs.len() as u64;
            if recs.len() < 2 { continue; }
            let first = recs.iter().map(|r| r.ts).fold(f64::INFINITY, f64::min);
            let last = recs.iter().map(|r| r.ts).fold(f64::NEG_INFINITY, f64::max);
            let span_min = ((last - first) / 60.0).max(1.0);
            // Clamp per-record u_ch — single records of 50k+ chars are
            // structurally not user typing, regardless of source.
            let total_u_ch: u64 = recs.iter().map(|r| r.u_ch.min(RECORD_U_CH_CAP)).sum();
            daily_rates.push(total_u_ch as f64 / span_min);
        }
        // Winsorize daily rates at p5/p95 before computing mean+std so a
        // single anomalous day can't blow up the comparison anchor or
        // inflate the standard deviation.
        let mut winsorized = daily_rates.clone();
        winsorize(&mut winsorized, 0.05, 0.95);
        let user_chars_per_min_mean = mean(&winsorized);
        let user_chars_per_min_std = stddev(&winsorized, user_chars_per_min_mean);
        // Robust statistics kept around for debug-scores readout / future use.
        let user_chars_per_min_med = median(&daily_rates);
        let user_chars_per_min_mad = mad(&daily_rates, user_chars_per_min_med);

        // Signal-tile distributions: per-day cc-based ratios.
        // Group ALL records (not just u_ch>0 ones) by date, then per day
        // compute cc/out, cc/u_ch (when u_ch>0), cc/active_min.
        let mut by_date_all: HashMap<(i32, u8, u8), Vec<&Record>> = HashMap::new();
        for r in records {
            let dt = time::OffsetDateTime::from_unix_timestamp(r.ts as i64)
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
                .to_offset(local_offset);
            by_date_all
                .entry((dt.year(), u8::from(dt.month()), dt.day()))
                .or_default()
                .push(r);
        }
        let mut investigations: Vec<f64> = Vec::new();
        let mut amplifications: Vec<f64> = Vec::new();
        let mut throughputs: Vec<f64> = Vec::new();
        for (_date, recs) in &by_date_all {
            if recs.len() < 5 { continue; } // skip days with too little signal
            let cc_sum: u64 = recs.iter().map(|r| r.cc).sum();
            let out_sum: u64 = recs.iter().map(|r| r.out).sum();
            let u_ch_sum: u64 = recs.iter().map(|r| r.u_ch).sum();
            if cc_sum == 0 { continue; }
            if out_sum > 0 {
                investigations.push(cc_sum as f64 / out_sum as f64);
            }
            if u_ch_sum > 0 {
                amplifications.push(cc_sum as f64 / u_ch_sum as f64);
            }
            let first = recs.iter().map(|r| r.ts).fold(f64::INFINITY, f64::min);
            let last = recs.iter().map(|r| r.ts).fold(f64::NEG_INFINITY, f64::max);
            let span_min = ((last - first) / 60.0).max(1.0);
            throughputs.push(cc_sum as f64 / span_min);
        }
        let investigation_med = median(&investigations);
        let investigation_mad = mad(&investigations, investigation_med);
        let amplification_med = median(&amplifications);
        let amplification_mad = mad(&amplifications, amplification_med);
        let throughput_med = median(&throughputs);
        let throughput_mad = mad(&throughputs, throughput_med);

        // Latency-tier percentiles (lat in ms across all records)
        let mut lats: Vec<f64> = records.iter().map(|r| r.lat as f64).collect();
        lats.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let lat_p20 = quantile(&mut lats.clone(), 0.20);
        let lat_p40 = quantile(&mut lats.clone(), 0.40);
        let lat_p60 = quantile(&mut lats.clone(), 0.60);
        let lat_p80 = quantile(&mut lats.clone(), 0.80);

        Self {
            n_records: records.len() as u64,
            n_sessions,
            out_med,
            out_mad,
            in_med,
            in_mad,
            ms_per_token_med,
            ms_per_token_mad,
            cache_miss_rate,
            session_out_cv_med,
            session_out_cv_mad,
            session_models_med,
            session_models_mad,
            gap_cv_med,
            gap_cv_mad,
            sessions_per_hour,
            user_chars_per_min_mean,
            user_chars_per_min_std,
            user_chars_per_min_med,
            user_chars_per_min_mad,
            n_records_with_u_ch,
            investigation_med,
            investigation_mad,
            amplification_med,
            amplification_mad,
            throughput_med,
            throughput_mad,
            lat_p20,
            lat_p40,
            lat_p60,
            lat_p80,
        }
    }
}

fn group_records_by_sid(records: &[Record]) -> HashMap<String, Vec<Record>> {
    let mut m: HashMap<String, Vec<Record>> = HashMap::new();
    for r in records {
        let sid = r.sid.clone().unwrap_or_else(|| "_orphan".into());
        m.entry(sid).or_default().push(r.clone());
    }
    m
}

/// Index-only group-by-sid (avoids cloning Records when the caller only
/// needs to walk indices into the original slice). Currently unused after
/// the per-day baseline switch but kept as a utility.
#[allow(dead_code)]
fn group_records_by_sid_basic(records: &[Record]) -> HashMap<String, Vec<usize>> {
    let mut m: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in records.iter().enumerate() {
        let sid = r.sid.clone().unwrap_or_else(|| "_orphan".into());
        m.entry(sid).or_default().push(i);
    }
    m
}

// ─── Driver-side (human-input kinetics) ──────────────────────────────────────
//
// Driver score measures the kinetic load the human is putting on the system:
// how many user-typed characters per minute are being produced, summed
// cumulatively across all active sessions in the window. Parallel sessions
// stack additively — driving 4 simultaneously is 4× the brain-burn, not
// the average per session.
//
// The metric ignores tool-loop continuations (which are bot-driven, not
// human-driven). It distinguishes them via the per-record u_ch field
// captured at request time: u_ch > 0 means the last user message of that
// request was plain text (fresh human input); u_ch == 0 means it was a
// tool_result (bot continuation) or the record predates the schema bump.
//
// When a window has too few new-schema records (< MIN_UCH_RECORDS), the
// driver score returns a neutral 50 with an "insufficient data" signal —
// it doesn't pretend to know.

const MIN_UCH_RECORDS_WINDOW: u64 = 1;
const MIN_UCH_RECORDS_BASELINE: u64 = 3;

/// Whether the driver-score baseline has accumulated enough new-schema
/// records to score against. Callers use this to render the driver tile
/// as "—" rather than a misleading neutral 50, and to omit the driver
/// line from charts entirely when bootstrapping.
pub fn driver_is_bootstrapping(baseline: &Baseline) -> bool {
    baseline.n_records_with_u_ch < MIN_UCH_RECORDS_BASELINE
}

fn driver_chars_per_min(a: &Aggregate) -> Option<f64> {
    // Sum u_ch within the window, divide by active span. Per-record u_ch
    // is clamped to RECORD_U_CH_CAP so a single anomalous record can't
    // dominate the rate (humans don't type tens-of-thousands of chars in
    // one API request — those are auto-injected blocks we haven't yet
    // strip-classified). Parallel sessions stack additively because we
    // sum across all records regardless of session.
    let total_u_ch: u64 = a.records.iter().map(|r| r.u_ch.min(RECORD_U_CH_CAP)).sum();
    let with_u_ch: u64 = a.records.iter().filter(|r| r.u_ch > 0).count() as u64;
    if with_u_ch < MIN_UCH_RECORDS_WINDOW {
        return None;
    }
    let first = a.first_ts?;
    let last = a.last_ts.unwrap_or(first);
    let span_min = ((last - first) / 60.0).max(1.0);
    Some(total_u_ch as f64 / span_min)
}

/// Sample-size confidence factor in [0, 1]. With few records the per-window
/// statistics (medians, CVs, z-scores) are essentially noise, and small-n
/// scores can land far from baseline by pure chance. Multiplying the
/// (raw_score − 50) excursion by this factor shrinks scores toward the
/// neutral 50 when n is small, and lets the raw score through once n
/// crosses the saturation threshold.
///
/// Linear ramp from 0 at n=0 to 1 at n=`SAMPLE_FULL`. Tuned so a window
/// with ≥ 50 records gets full weight; a window with 5 records gets only
/// 10% of the deviation.
const SAMPLE_FULL: f64 = 50.0;

fn confidence(n: u64) -> f64 {
    (n as f64 / SAMPLE_FULL).clamp(0.0, 1.0)
}

fn shrink(raw_score: f64, confidence: f64) -> f64 {
    50.0 + (raw_score - 50.0) * confidence
}

pub fn driver_score(a: &Aggregate, baseline: &Baseline) -> u32 {
    if a.n == 0 || baseline.n_records == 0 {
        return 0;
    }
    // Insufficient new-schema baseline → can't z-score against history.
    // Insufficient new-schema window → can't compute current rate.
    // Both cases return neutral 50.
    if baseline.n_records_with_u_ch < MIN_UCH_RECORDS_BASELINE {
        return 50;
    }
    let Some(cur_cpm) = driver_chars_per_min(a) else {
        return 50;
    };
    // Use winsorized mean + std for the comparison. Std-floor at 20% of
    // mean (or 5 c/min absolute) keeps z bounded when the user's history
    // is unusually consistent — without it a very tight baseline lets any
    // small deviation saturate. Logistic scale = 1.5 so z=±2 ≈ score 90/10
    // and z=±4 saturates near 0/100.
    let std_floor = (baseline.user_chars_per_min_mean * 0.20).max(5.0);
    let std = baseline.user_chars_per_min_std.max(std_floor);
    let z = if std > 1e-9 {
        (cur_cpm - baseline.user_chars_per_min_mean) / std
    } else {
        0.0
    };
    let raw = logistic_score(z, 1.5);
    let shrunk = shrink(raw, confidence(a.n));
    shrunk.round().clamp(0.0, 100.0) as u32
}

// ─── Bot-side (output-health-focused) ────────────────────────────────────────
//
// Bot score measures the QUALITY/HEALTH of the bot's outputs and its
// streaming behavior, NOT the upstream API's tail latency. Four components:
//
//   brevity    — median output tokens vs typical (low = bot bailing)
//   stalling   — ms per output token vs typical (high = streaming choke)
//   wandering  — within-session output variance vs typical (high = unstable)
//   cache_drag — cache miss rate vs typical (high = no cache benefit)

fn bot_brevity(a: &Aggregate, baseline: &Baseline) -> f64 {
    if a.records.is_empty() {
        return 50.0;
    }
    let outs: Vec<f64> = a.records.iter().map(|r| r.out as f64).collect();
    let cur = median(&outs);
    // Concerning when current is BELOW baseline → swap sign of diff.
    let z = robust_z(baseline.out_med - cur, 0.0, baseline.out_mad);
    logistic_score(z, 1.5)
}

fn bot_stalling(a: &Aggregate, baseline: &Baseline) -> f64 {
    let ms_per_token: Vec<f64> = a
        .records
        .iter()
        .filter(|r| r.out > 0)
        .map(|r| r.lat as f64 / r.out as f64)
        .collect();
    if ms_per_token.is_empty() {
        return 50.0;
    }
    let cur = median(&ms_per_token);
    // Concerning when current is ABOVE baseline.
    let z = robust_z(cur, baseline.ms_per_token_med, baseline.ms_per_token_mad);
    logistic_score(z, 1.5)
}

fn bot_wandering(a: &Aggregate, baseline: &Baseline) -> f64 {
    let by_sid = group_records_by_sid(&a.records);
    let cvs: Vec<f64> = by_sid
        .values()
        .filter(|recs| recs.len() >= 3)
        .map(|recs| {
            let outs: Vec<f64> = recs.iter().map(|r| r.out as f64).collect();
            cv(&outs)
        })
        .collect();
    if cvs.is_empty() {
        return 50.0;
    }
    let cur = median(&cvs);
    let z = robust_z(cur, baseline.session_out_cv_med, baseline.session_out_cv_mad);
    logistic_score(z, 1.5)
}

fn bot_cache_drag(a: &Aggregate, baseline: &Baseline) -> f64 {
    let total_cr: u64 = a.records.iter().map(|r| r.cr).sum();
    let total_cc: u64 = a.records.iter().map(|r| r.cc).sum();
    if total_cr + total_cc == 0 {
        return 50.0;
    }
    let cur = total_cc as f64 / (total_cr + total_cc) as f64;
    let synth_mad = (baseline.cache_miss_rate * 0.3).max(0.05);
    let z = robust_z(cur, baseline.cache_miss_rate, synth_mad);
    logistic_score(z, 1.5)
}

pub fn bot_score(a: &Aggregate, baseline: &Baseline) -> u32 {
    if a.n == 0 || baseline.n_records == 0 {
        return 0;
    }
    let brevity = bot_brevity(a, baseline);
    let stalling = bot_stalling(a, baseline);
    let wandering = bot_wandering(a, baseline);
    let cache_drag = bot_cache_drag(a, baseline);
    let composite =
        brevity * 0.35 + stalling * 0.25 + wandering * 0.25 + cache_drag * 0.15;
    let shrunk = shrink(composite, confidence(a.n));
    shrunk.round().clamp(0.0, 100.0) as u32
}

/// Diagnostic dump of every score component for one window. Use it to
/// validate that the headline numbers come from the components you expect.
pub fn score_breakdown(
    a: &Aggregate,
    baseline: &Baseline,
) -> ScoreBreakdown {
    let conf = confidence(a.n);

    // Driver: kinetic chars/min vs baseline mean (winsorized).
    let total_u_ch: u64 = a.records.iter().map(|r| r.u_ch.min(RECORD_U_CH_CAP)).sum();
    let with_u_ch: u64 = a.records.iter().filter(|r| r.u_ch > 0).count() as u64;
    let cur_cpm = driver_chars_per_min(a).unwrap_or(0.0);
    let std_floor = (baseline.user_chars_per_min_mean * 0.20).max(5.0);
    let used_std = baseline.user_chars_per_min_std.max(std_floor);
    let driver_z = if used_std > 1e-9 {
        (cur_cpm - baseline.user_chars_per_min_mean) / used_std
    } else {
        0.0
    };
    let d_raw = logistic_score(driver_z, 1.5);
    let d_shrunk = shrink(d_raw, conf);

    let b_brevity = bot_brevity(a, baseline);
    let b_stalling = bot_stalling(a, baseline);
    let b_wandering = bot_wandering(a, baseline);
    let b_cache_drag = bot_cache_drag(a, baseline);
    let b_raw =
        b_brevity * 0.35 + b_stalling * 0.25 + b_wandering * 0.25 + b_cache_drag * 0.15;

    ScoreBreakdown {
        n: a.n,
        confidence: conf,
        d_total_u_ch: total_u_ch,
        d_with_u_ch: with_u_ch,
        d_chars_per_min: cur_cpm,
        d_baseline_cpm: baseline.user_chars_per_min_mean,
        d_baseline_mad: used_std,
        d_z: driver_z,
        d_raw, d_shrunk,
        b_brevity, b_stalling, b_wandering, b_cache_drag,
        b_raw, b_shrunk: shrink(b_raw, conf),
    }
}

#[derive(Debug)]
pub struct ScoreBreakdown {
    pub n: u64,
    pub confidence: f64,
    pub d_total_u_ch: u64,
    pub d_with_u_ch: u64,
    pub d_chars_per_min: f64,
    pub d_baseline_cpm: f64,
    pub d_baseline_mad: f64,
    pub d_z: f64,
    pub d_raw: f64,
    pub d_shrunk: f64,
    pub b_brevity: f64,
    pub b_stalling: f64,
    pub b_wandering: f64,
    pub b_cache_drag: f64,
    pub b_raw: f64,
    pub b_shrunk: f64,
}

// ─── Signal heuristic ────────────────────────────────────────────────────────
//
// Computes three cc-based ratios for the current window, z-scores each
// against per-day baseline distributions, picks the dominant deviation
// (max |z|), and maps to a phrase describing what the agent is doing
// right now.
//
//   investigation = cc / out          (reading vs writing density)
//   amplification = cc / u_ch         (each typed char → tokens loaded)
//   throughput    = cc / active_min   (raw new-context rate)
//
// Returns a `Signal` with the dominant phrase + value. When no metric
// crosses the 1σ threshold, returns "steady".

#[derive(Debug, Clone)]
pub struct Signal {
    pub phrase: &'static str,
    /// Short human-readable representation of the dominant ratio's value.
    pub value: String,
    /// Dominant z-score (signed).
    pub z: f64,
}

pub fn compute_signal(a: &Aggregate, baseline: &Baseline) -> Signal {
    let cc_sum: u64 = a.records.iter().map(|r| r.cc).sum();
    let out_sum: u64 = a.records.iter().map(|r| r.out).sum();
    let u_ch_sum: u64 = a.records.iter().map(|r| r.u_ch).sum();
    let span_min = match (a.first_ts, a.last_ts) {
        (Some(f), Some(l)) if l > f => ((l - f) / 60.0).max(1.0),
        _ => 1.0,
    };

    if a.records.is_empty() || cc_sum == 0 {
        return Signal { phrase: "—", value: "no signal".into(), z: 0.0 };
    }

    // Each candidate: (name, cur, base_med, base_mad, mad_floor, value-fmt-fn)
    let inv_cur = if out_sum > 0 { Some(cc_sum as f64 / out_sum as f64) } else { None };
    let amp_cur = if u_ch_sum > 0 { Some(cc_sum as f64 / u_ch_sum as f64) } else { None };
    let thr_cur = Some(cc_sum as f64 / span_min);

    // Each candidate: (cur, med, mad, mad_floor, low_phrase, high_phrase, value-fmt)
    let mut candidates: Vec<(f64, f64, f64, f64, &'static str, &'static str, String)> = Vec::new();
    if let Some(c) = inv_cur {
        if baseline.investigation_med > 0.0 {
            candidates.push((
                c,
                baseline.investigation_med,
                baseline.investigation_mad,
                (baseline.investigation_med * 0.20).max(0.05),
                "synthesis",
                "deep dive",
                format!("{:.2}", c),
            ));
        }
    }
    if let Some(c) = amp_cur {
        if baseline.amplification_med > 0.0 {
            candidates.push((
                c,
                baseline.amplification_med,
                baseline.amplification_mad,
                (baseline.amplification_med * 0.20).max(0.5),
                "manual",
                "amplified",
                format!("{:.0}×", c),
            ));
        }
    }
    if let Some(c) = thr_cur {
        if baseline.throughput_med > 0.0 {
            candidates.push((
                c,
                baseline.throughput_med,
                baseline.throughput_mad,
                (baseline.throughput_med * 0.20).max(10.0),
                "context calm",
                "context surge",
                format!("{:.0}/m", c),
            ));
        }
    }

    if candidates.is_empty() {
        return Signal { phrase: "—", value: "warming up".into(), z: 0.0 };
    }

    // Compute z for each (using max(baseline_mad, mad_floor) as denominator),
    // pick max |z|.
    let mut best: Option<(f64, &'static str, &'static str, String)> = None;
    for (cur, med, mad, floor, low, high, fmt) in &candidates {
        let denom = mad.max(*floor);
        let z = if denom > 1e-9 { (cur - med) / denom } else { 0.0 };
        let cur_best_abs = best.as_ref().map(|(z, _, _, _)| z.abs()).unwrap_or(0.0);
        if z.abs() > cur_best_abs {
            best = Some((z, *low, *high, fmt.clone()));
        }
    }
    let (z, low, high, value) = best.unwrap();
    let phrase = if z.abs() < 1.0 {
        "steady"
    } else if z > 0.0 {
        high
    } else {
        low
    };
    Signal { phrase, value, z }
}

// ─── Labels ──────────────────────────────────────────────────────────────────

pub fn vibe_label(score: u32) -> &'static str {
    match score {
        s if s < 20 => "crisp 🧊",
        s if s < 40 => "fine",
        s if s < 60 => "mid",
        s if s < 80 => "cooked 🔥",
        _ => "fried 💀",
    }
}

pub fn diagnosis(bot: u32, driver: u32) -> Option<&'static str> {
    if bot < 30 && driver < 30 {
        return None;
    }
    let diff = bot.abs_diff(driver);
    let avg = (bot + driver) / 2;
    if diff < 15 {
        if avg > 60 {
            return Some("co-rotting — driver and bot are in a feedback loop");
        }
        if avg > 40 {
            return Some("drift on both sides; nothing alarming yet");
        }
        return None;
    }
    if driver > bot {
        if driver > 70 {
            return Some("driver is rotting; bot is keeping up. throttle, refocus.");
        }
        if driver > 50 {
            return Some("prompts are bloating or driver is rapid-firing");
        }
        return Some("driver-side drift; bot is fine");
    }
    if bot > 70 {
        return Some("bot is cooked. swap models or clear context.");
    }
    if bot > 50 {
        return Some("bot output is shrinking or latency is climbing");
    }
    Some("bot-side drift; driver is clean")
}

pub fn short_model(m: &str) -> String {
    if m.is_empty() {
        return "unknown".into();
    }
    let stripped = m.strip_prefix("claude-").unwrap_or(m);
    let parts: Vec<&str> = stripped.split('-').collect();
    if parts.is_empty() {
        return m.into();
    }
    let name = parts[0];
    let ver = if parts.len() >= 3 && parts[1].chars().all(|c| c.is_ascii_digit()) {
        if parts[2].chars().all(|c| c.is_ascii_digit()) {
            format!("-{}.{}", parts[1], parts[2])
        } else {
            format!("-{}", parts[1])
        }
    } else if parts.len() >= 2 && parts[1].chars().all(|c| c.is_ascii_digit()) {
        format!("-{}", parts[1])
    } else {
        String::new()
    };
    format!("{}{}", name, ver)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ts: f64, sid: &str) -> Record {
        Record {
            ts,
            te: ts + 0.001, // response end ~ request start so gaps ≈ g
            model: Some("claude-3-5-sonnet".into()),
            sid: Some(sid.into()),
            r#in: 10,
            out: 20,
            tot: 30,
            lat: 1000,
            cr: 0,
            cc: 0,
            c_us: None,
            u_ch: 0,
            tr_ch: 0,
            lex_div: 0.0,
            fn_word_frac: 0.0,
            ngram_entropy: 0.0,
            nvt: 0.0,
        }
    }

    /// A plain-text user turn with explicit wordology features (u_ch chars,
    /// lex_div / fn_word_frac / ngram_entropy / nvt novelty).
    fn rec_lex(ts: f64, sid: &str, u_ch: u64, lex_div: f64, fnw: f64, nge: f64, nvt: f64) -> Record {
        let mut r = rec(ts, sid);
        r.u_ch = u_ch;
        r.lex_div = lex_div;
        r.fn_word_frac = fnw;
        r.ngram_entropy = nge;
        r.nvt = nvt;
        r
    }

    /// Build a session as (start_ts, [gap, kind...]): each gap is seconds
    /// after the previous response end; `kind` is the *expected* label.
    fn session(gaps: &[(f64, TurnKind)]) -> Vec<Record> {
        let mut out = Vec::new();
        let mut ts = 0.0;
        out.push(rec(ts, "s1"));
        for (g, _k) in gaps {
            ts += *g;
            out.push(rec(ts, "s1"));
        }
        out
    }

    fn bot_count(kinds: &[TurnKind]) -> usize {
        kinds.iter().filter(|k| **k == TurnKind::Bot).count()
    }

    #[test]
    fn clean_fast_loops_agree_with_deterministic() {
        // Fast tool loops (0.5–2s) + long human turns (30–60s): the hardcoded
        // 5s cut and the learned boundary should both see the same split.
        let recs = session(&[
            (0.5, TurnKind::Bot),
            (1.0, TurnKind::Bot),
            (2.0, TurnKind::Bot),
            (45.0, TurnKind::Driver),
            (1.5, TurnKind::Bot),
            (1.0, TurnKind::Bot),
            (60.0, TurnKind::Driver),
        ]);
        let det = classify_turns(&recs);
        let (prob, mix) = classify_turns_prob_with_model(&recs);
        let mix = mix.expect("fit should succeed");
        assert_eq!(bot_count(&det), bot_count(&prob), "clean case: counts should match");
        assert!(mix.mu_bot.exp() < 5.0, "bot median should be ~1-2s");
        assert!(mix.mu_drv.exp() > 20.0, "driver median should be tens of seconds");
        assert!(
            mix.mu_bot.exp() < mix.threshold_sec && mix.threshold_sec < mix.mu_drv.exp(),
            "learned boundary should sit between the two modes"
        );
        // The learned boundary is *this user's* crossover, not a magic 5s: it
        // must sit between the two fitted modes. (With all loops <2s and turns
        // ~50s it legitimately lands ~20s — gaps up to there are still bot.)
        assert_eq!(prob[0], TurnKind::Driver, "first turn is always Driver");
    }

    #[test]
    fn slow_tool_loops_fix_deterministic_mislabel() {
        // The money test: a "slow tool" session where every loop gap is ~8s.
        // The deterministic classifier (gap > 5s ⇒ Driver) labels ALL of them
        // as Driver — it can't see a bot cluster at all. The learned mixture
        // should find the ~8s bot component and correctly reclassify them.
        let recs = session(&[
            (8.0, TurnKind::Bot),
            (8.0, TurnKind::Bot),
            (8.0, TurnKind::Bot),
            (8.0, TurnKind::Bot),
            (60.0, TurnKind::Driver),
            (8.0, TurnKind::Bot),
            (8.0, TurnKind::Bot),
            (90.0, TurnKind::Driver),
        ]);
        let det = classify_turns(&recs);
        let (prob, mix) = classify_turns_prob_with_model(&recs);
        let mix = mix.expect("median-split warm start should fit");
        assert_eq!(bot_count(&det), 0, "deterministic sees no bot cluster here");
        assert!(bot_count(&prob) > 0, "probabilistic should recover the bot cluster");
        assert!(
            mix.threshold_sec > 5.0 && mix.threshold_sec < 60.0,
            "learned boundary should sit between the 8s and 60s modes, got {}",
            mix.threshold_sec
        );
        assert!((mix.mu_bot.exp() - 8.0).abs() < 3.0, "bot median ≈ 8s");
    }

    #[test]
    fn degenerate_data_falls_back_to_deterministic() {
        // All gaps nearly identical → no stable 2-component split → must fall
        // back to the deterministic classifier (never panic, never invent).
        let recs = session(&[
            (3.0, TurnKind::Bot),
            (3.0, TurnKind::Bot),
            (3.0, TurnKind::Bot),
            (3.0, TurnKind::Bot),
        ]);
        let det = classify_turns(&recs);
        let (prob, mix) = classify_turns_prob_with_model(&recs);
        assert!(mix.is_none(), "degenerate data should refuse the fit");
        assert_eq!(bot_count(&det), bot_count(&prob), "fallback = deterministic");
        assert_eq!(prob[0], TurnKind::Driver);
    }

    #[test]
    fn empty_input_is_safe() {
        let (kinds, mix) = classify_turns_prob_with_model(&[]);
        assert!(kinds.is_empty());
        assert!(mix.is_none());
    }

    #[test]
    fn wordology_rescues_all_slow_bot_pacing() {
        // The money case for the second (wordology) axis: a bot controls the
        // prompting, paces like a human (every gap slow), and writes
        // machine-flat plain text. Gap alone can't see it — the deterministic
        // classifier says all Driver and there's no fast cluster to fit a
        // mixture — but the lexical signal forces Bot on the flat text.
        let mut recs = vec![rec(0.0, "s1")];
        let mut ts = 0.0;
        for _ in 0..3 {
            ts += 12.0;
            // machine-flat: low TTR, low n-gram entropy, low fn-word fraction
            recs.push(rec_lex(ts, "s1", 90, 0.25, 0.20, 1.2, 0.0));
        }
        let det = classify_turns(&recs);
        let (prob, _mix) = classify_turns_prob_with_model(&recs);
        assert_eq!(bot_count(&det), 0, "gap-only sees no bot cluster here");
        assert!(bot_count(&prob) > 0, "wordology should force the bot turns");
        for i in 1..recs.len() {
            assert_eq!(prob[i], TurnKind::Bot, "machine-flat slow turn must be Bot");
        }
    }

    #[test]
    fn wordology_human_words_keep_fast_turns_driver() {
        // Reverse correction: a fast typist (short gaps, so pacing says Bot)
        // whose words are clearly human should stay a Driver.
        let mut recs = vec![rec(0.0, "s1")];
        let mut ts = 0.0;
        for _ in 0..3 {
            ts += 1.5;
            // human-varied: high TTR, high n-gram entropy, high fn-word frac
            recs.push(rec_lex(ts, "s1", 120, 0.85, 0.60, 4.2, 0.0));
        }
        let det = classify_turns(&recs);
        let (prob, _mix) = classify_turns_prob_with_model(&recs);
        assert_eq!(bot_count(&det), 3, "gap-only calls a fast typist a bot");
        assert_eq!(bot_count(&prob), 0, "wordology keeps a fast typist a driver");
    }

    #[test]
    fn wordology_tool_result_forces_bot_on_slow_loop() {
        // tr_ch>0 is bot feedback by construction; even a very slow loop must
        // be Bot. This is the lexical axis rescuing the deterministic 5s rule.
        let mut recs = vec![rec(0.0, "s1")];
        let mut ts = 0.0;
        for _ in 0..3 {
            ts += 30.0;
            let mut r = rec(ts, "s1");
            r.tr_ch = 500;
            recs.push(r);
        }
        let det = classify_turns(&recs);
        let (prob, _mix) = classify_turns_prob_with_model(&recs);
        assert_eq!(bot_count(&det), 0, "gap-only sees slow tool loops as driver");
        assert_eq!(bot_count(&prob), 3, "tr_ch forces bot regardless of pacing");
    }

    #[test]
    fn wordology_cross_turn_template_reuse_rescues_slow_bot() {
        // A bot repeats the same template across turns: per-message features
        // are only mid (not confident alone), but nvt ~0.9 (near-total reuse)
        // is the smoking gun. Gap sees slow pacing ⇒ Driver; momentum forces
        // Bot — this is the case per-message judgement alone would dodge.
        let mut recs = vec![rec(0.0, "s1")];
        let mut ts = 0.0;
        for _ in 0..3 {
            ts += 12.0;
            // mid per-message (lex_div .35, nge 1.8, fnw .28) + near-total reuse
            recs.push(rec_lex(ts, "s1", 90, 0.35, 0.28, 1.8, 0.9));
        }
        let det = classify_turns(&recs);
        let (prob, _mix) = classify_turns_prob_with_model(&recs);
        assert_eq!(bot_count(&det), 0, "gap-only sees template reuse as driver");
        for i in 1..recs.len() {
            assert_eq!(prob[i], TurnKind::Bot, "template reuse must force Bot");
        }
    }

    #[test]
    fn wordology_uniform_sizes_mark_template_bot_but_bursty_stays_driver() {
        // Same mid per-message text, same slow pacing, same turn count: the
        // ONLY difference is burstiness. Uniform template-sized prompts ⇒ Bot;
        // bursty human-sized prompts ⇒ Driver. That's momentum, not per-message.
        let mut uniform = vec![rec(0.0, "u")];
        let mut ts = 0.0;
        for _ in 0..4 {
            ts += 12.0;
            uniform.push(rec_lex(ts, "u", 90, 0.35, 0.28, 1.8, 0.0));
        }
        let mut bursty = vec![rec(0.0, "b")];
        ts = 0.0;
        for sz in [45u64, 80, 150, 200] {
            ts += 12.0;
            bursty.push(rec_lex(ts, "b", sz, 0.35, 0.28, 1.8, 0.0));
        }
        let (u_prob, _) = classify_turns_prob_with_model(&uniform);
        let (b_prob, _) = classify_turns_prob_with_model(&bursty);
        for i in 1..uniform.len() {
            assert_eq!(u_prob[i], TurnKind::Bot, "uniform template sizes ⇒ Bot");
        }
        for i in 1..bursty.len() {
            assert_eq!(b_prob[i], TurnKind::Driver, "bursty human sizes ⇒ Driver");
        }
    }

    #[test]
    fn posterior_is_a_probability() {
        // Sanity: posterior_bot is bounded [0,1] and monotone across a band.
        let p_small = posterior_bot(1.0, 0.5, 0.6, 0.7, 3.5, 0.9, 0.3);
        let p_mid = posterior_bot(20.0, 0.5, 0.6, 0.7, 3.5, 0.9, 0.3);
        let p_big = posterior_bot(200.0, 0.5, 0.6, 0.7, 3.5, 0.9, 0.3);
        assert!((0.0..=1.0).contains(&p_small) && (0.0..=1.0).contains(&p_mid) && (0.0..=1.0).contains(&p_big));
        assert!(p_small > p_mid && p_mid > p_big, "short gap ⇒ more likely bot");
    }
}
