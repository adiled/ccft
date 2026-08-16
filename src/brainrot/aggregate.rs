//! Record aggregation + bot/driver scoring (V·L·P·V model). Port of cc-flytrap/brainrot.py.

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
            if r.lat > a.lat_max {
                a.lat_max = r.lat;
            }
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TurnKind {
    Driver,
    Bot,
}

pub const BOT_LOOP_THRESHOLD: f64 = 5.0;

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

const LN_2PI_HALF: f64 = 0.9189385332046727; // ln(2π)/2, log-normal normalizer
const MIN_GAPS_PER_COMPONENT: usize = 2; // refuses fit if component too small
const MIN_PRIOR: f64 = 0.02; // below this → component collapse → fallback
const SIGMA_FLOOR: f64 = 0.1; // prevents component collapsing to a point
const EM_MAX_ITERS: u32 = 120;
const EM_TOL: f64 = 1e-7;

const MIN_LEX_CHARS: u64 = 40;

/// 2-component log-normal gap mixture.
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

pub fn fit_gap_mixture(records: &[Record]) -> Option<GapMixture> {
    let gaps = pooled_gaps(records);
    let n = gaps.len();
    if n < MIN_GAPS_PER_COMPONENT * 2 {
        return None;
    }

    let split = |cut: f64| {
        (
            gaps.iter()
                .copied()
                .filter(|g| *g <= cut)
                .collect::<Vec<f64>>(),
            gaps.iter()
                .copied()
                .filter(|g| *g > cut)
                .collect::<Vec<f64>>(),
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
        pi_b = s_b / n as f64;
        pi_d = s_d / n as f64;
        if pi_b < MIN_PRIOR || pi_d < MIN_PRIOR {
            return None;
        }
        mu_b = s_b_ln / s_b;
        mu_d = s_d_ln / s_d;
        sigma_b = (s_b_l2 / s_b - mu_b * mu_b)
            .max(0.0)
            .sqrt()
            .max(SIGMA_FLOOR);
        sigma_d = (s_d_l2 / s_d - mu_d * mu_d)
            .max(0.0)
            .sqrt()
            .max(SIGMA_FLOOR);
        let dll = new_ll - ll;
        ll = new_ll;
        if dll.abs() < EM_TOL * (1.0 + ll.abs()) {
            break;
        }
    }

    // Keep "bot" = short-gap component (guard against swapped convergence).
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

#[derive(Clone, Copy, Default)]
struct MomCtx {
    cv_size: f64,
}

fn machine_likeness(r: &Record, mom: &MomCtx) -> Option<f64> {
    if r.tr_ch > 0 {
        return Some(1.0);
    }
    if r.u_ch < MIN_LEX_CHARS || r.lex_div <= 0.0 {
        return None;
    }
    let p_nge = 1.0 - (r.ngram_entropy / 4.0).clamp(0.0, 1.0);
    let p_ttr = 1.0 - r.lex_div.clamp(0.0, 1.0);
    let p_fnx = 1.0 - (r.fn_word_frac / 0.5).clamp(0.0, 1.0);
    let mut p = 0.5 * p_ttr + 0.35 * p_nge + 0.15 * p_fnx;
    if p > 0.5 {
        if r.nvt > 0.0 {
            p = 0.5 * p + 0.5 * r.nvt.clamp(0.0, 1.0);
        }
        if mom.cv_size >= 0.0 {
            let p_burst = 1.0 - (mom.cv_size / 0.5).clamp(0.0, 1.0);
            p = 0.7 * p + 0.3 * p_burst;
        }
    }
    Some(p.clamp(0.0, 1.0))
}

pub fn classify_turns_prob(records: &[Record]) -> Vec<TurnKind> {
    classify_turns_prob_with_model(records).0
}

/// As above but also returns the fitted model. Unused by callers except tests.
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
        let sizes: Vec<f64> = idxs
            .iter()
            .filter(|i| records[**i].u_ch > 0)
            .map(|i| records[*i].u_ch as f64)
            .collect();
        let cv_size = if sizes.len() >= 2 {
            let mean = sizes.iter().sum::<f64>() / sizes.len() as f64;
            if mean > 0.0 {
                let var =
                    sizes.iter().map(|s| (s - mean) * (s - mean)).sum::<f64>() / sizes.len() as f64;
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
            if let Some(p_lex) = machine_likeness(r, &mom) {
                if p_lex >= 0.65 {
                    p = p.max(0.75);
                } else if p_lex <= 0.25 {
                    p = p.min(0.25);
                } else {
                    p = 0.6 * p + 0.4 * p_lex;
                }
            }
            kinds[*i] = if p > 0.5 {
                TurnKind::Bot
            } else {
                TurnKind::Driver
            };
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

/// OLS slope, 0 when degenerate. Unused but cheap.
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

/// MAD scaled to stdev comparability.
fn mad(xs: &[f64], med: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let dev: Vec<f64> = xs.iter().map(|x| (x - med).abs()).collect();
    median(&dev) * 1.4826
}

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

fn robust_z(x: f64, med: f64, mad: f64) -> f64 {
    if mad <= 1e-9 {
        return 0.0;
    }
    (x - med) / mad
}

fn logistic_score(z: f64, scale: f64) -> f64 {
    50.0 + 50.0 * (z / scale).tanh()
}

const RECORD_U_CH_CAP: u64 = 5000;

fn winsorize(xs: &mut [f64], p_low: f64, p_high: f64) -> (f64, f64) {
    if xs.len() < 4 {
        return (
            xs.iter().cloned().fold(f64::INFINITY, f64::min),
            xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        );
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

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

fn stddev(xs: &[f64], m: f64) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let var = xs.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (xs.len() - 1) as f64;
    var.sqrt()
}

// Baseline (user fingerprint).

#[derive(Default, Debug, Clone)]
pub struct Baseline {
    pub n_records: u64,
    pub n_sessions: usize,

    pub out_med: f64,
    pub out_mad: f64,
    pub in_med: f64,
    pub in_mad: f64,
    pub ms_per_token_med: f64,
    pub ms_per_token_mad: f64,

    pub cache_miss_rate: f64,

    pub session_out_cv_med: f64,
    pub session_out_cv_mad: f64,
    pub session_models_med: f64,
    pub session_models_mad: f64,
    pub gap_cv_med: f64,
    pub gap_cv_mad: f64,

    pub sessions_per_hour: f64,

    pub user_chars_per_min_mean: f64,
    pub user_chars_per_min_std: f64,
    pub user_chars_per_min_med: f64, // back-compat: same data as mean/std
    pub user_chars_per_min_mad: f64,
    pub n_records_with_u_ch: u64,

    pub investigation_med: f64, // cc / out per day
    pub investigation_mad: f64,
    pub amplification_med: f64, // cc / u_ch per day (when u_ch > 0)
    pub amplification_mad: f64,
    pub throughput_med: f64, // cc / active_minutes per day
    pub throughput_mad: f64,

    pub lat_p20: f64,
    pub lat_p40: f64,
    pub lat_p60: f64,
    pub lat_p80: f64,

    // Thinking rot: median combined signal (th_ch/out * (1 - lex_div)).
    // High values = heavy thinking + repetitive output = bot rot.
    pub thinking_rot_med: f64,
    pub thinking_rot_mad: f64,
}

impl Baseline {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_records(records: &[Record]) -> Self {
        if records.is_empty() {
            return Self::default();
        }

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

        let total_cr: u64 = records.iter().map(|r| r.cr).sum();
        let total_cc: u64 = records.iter().map(|r| r.cc).sum();
        let cache_miss_rate = if total_cr + total_cc > 0 {
            total_cc as f64 / (total_cr + total_cc) as f64
        } else {
            0.0
        };

        let by_sid = group_records_by_sid(records);
        let n_sessions = by_sid.len();

        let mut session_out_cvs: Vec<f64> = Vec::new();
        let mut session_models: Vec<f64> = Vec::new();
        let mut session_gap_cvs: Vec<f64> = Vec::new();

        for recs in by_sid.values() {
            let outs: Vec<f64> = recs.iter().map(|r| r.out as f64).collect();
            session_out_cvs.push(cv(&outs));

            let mut models: HashSet<String> = HashSet::new();
            for r in recs {
                if let Some(m) = &r.model {
                    models.insert(m.clone());
                }
            }
            session_models.push(models.len() as f64);

            let mut sorted = recs.clone();
            sorted.sort_by(|a, b| a.ts.partial_cmp(&b.ts).unwrap_or(std::cmp::Ordering::Equal));
            if sorted.len() >= 3 {
                let gaps: Vec<f64> = sorted.windows(2).map(|w| w[1].ts - w[0].ts).collect();
                session_gap_cvs.push(cv(&gaps));
            }
        }

        let session_out_cv_med = median(&session_out_cvs);
        let session_out_cv_mad = mad(&session_out_cvs, session_out_cv_med);
        let session_models_med = median(&session_models);
        let session_models_mad = mad(&session_models, session_models_med);
        let gap_cv_med = median(&session_gap_cvs);
        let gap_cv_mad = mad(&session_gap_cvs, gap_cv_med);

        let first_ts = records.iter().map(|r| r.ts).fold(f64::INFINITY, f64::min);
        let last_ts = records
            .iter()
            .map(|r| r.ts)
            .fold(f64::NEG_INFINITY, f64::max);
        let span_hours = ((last_ts - first_ts) / 3600.0).max(1.0 / 60.0);
        let sessions_per_hour = n_sessions as f64 / span_hours;

        let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
        let mut by_date: HashMap<(i32, u8, u8), Vec<&Record>> = HashMap::new();
        for r in records {
            if r.u_ch == 0 {
                continue;
            }
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
            if recs.len() < 2 {
                continue;
            }
            let first = recs.iter().map(|r| r.ts).fold(f64::INFINITY, f64::min);
            let last = recs.iter().map(|r| r.ts).fold(f64::NEG_INFINITY, f64::max);
            let span_min = ((last - first) / 60.0).max(1.0);
            let total_u_ch: u64 = recs.iter().map(|r| r.u_ch.min(RECORD_U_CH_CAP)).sum();
            daily_rates.push(total_u_ch as f64 / span_min);
        }
        let mut winsorized = daily_rates.clone();
        winsorize(&mut winsorized, 0.05, 0.95);
        let user_chars_per_min_mean = mean(&winsorized);
        let user_chars_per_min_std = stddev(&winsorized, user_chars_per_min_mean);
        let user_chars_per_min_med = median(&daily_rates);
        let user_chars_per_min_mad = mad(&daily_rates, user_chars_per_min_med);

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
            if recs.len() < 5 {
                continue;
            } // skip days with too little signal
            let cc_sum: u64 = recs.iter().map(|r| r.cc).sum();
            let out_sum: u64 = recs.iter().map(|r| r.out).sum();
            let u_ch_sum: u64 = recs.iter().map(|r| r.u_ch).sum();
            if cc_sum == 0 {
                continue;
            }
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

        let mut lats: Vec<f64> = records.iter().map(|r| r.lat as f64).collect();
        lats.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let lat_p20 = quantile(&mut lats.clone(), 0.20);
        let lat_p40 = quantile(&mut lats.clone(), 0.40);
        let lat_p60 = quantile(&mut lats.clone(), 0.60);
        let lat_p80 = quantile(&mut lats.clone(), 0.80);

        // ── Thinking rot signal ──────────────────────────────────────────
        // Rot = heavy hidden reasoning + repetitive output. Store `th_ch/out
        // * lex_div` so low values mean "massive thinking, repetitive words"
        let mut thinking_rot_vals: Vec<f64> = Vec::new();
        for r in records {
            if r.out == 0 || r.th_ch == 0 || r.lex_div <= 0.0 {
                continue;
            }
            // normalize lex_div → inverse (rot axis) * ratio
            let rot = (r.th_ch as f64 / r.out as f64) * (1.0 - r.lex_div.clamp(0.0, 1.0));
            thinking_rot_vals.push(rot);
        }
        let thinking_rot_med = median(&thinking_rot_vals);
        let thinking_rot_mad = mad(&thinking_rot_vals, thinking_rot_med);

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
            thinking_rot_med,
            thinking_rot_mad,
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

/// Index-only group-by-sid. Unused but kept.
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

pub fn driver_is_bootstrapping(baseline: &Baseline) -> bool {
    baseline.n_records_with_u_ch < MIN_UCH_RECORDS_BASELINE
}

fn driver_chars_per_min(a: &Aggregate) -> Option<f64> {
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
    if baseline.n_records_with_u_ch < MIN_UCH_RECORDS_BASELINE {
        return 50;
    }
    let Some(cur_cpm) = driver_chars_per_min(a) else {
        return 50;
    };
    // Std-floor at 20% of mean keeps z bounded on tight baselines. Scale=1.5 gives z=±2 → 90/10.
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
    let z = robust_z(
        cur,
        baseline.session_out_cv_med,
        baseline.session_out_cv_mad,
    );
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

// ── Thinking rot ───────────────────────────────────────────────────────────────
//
// Detects the pattern: heavy hidden reasoning + repetitive bot output.
// The metric is the median of `th_ch / out` across records. A high value
// means the bot is thinking far more than it's delivering — classic rot.
// A large deviation above baseline's median is the strongest signal
// (the "spinning wheels" pattern).

fn bot_thinking_rot(a: &Aggregate, baseline: &Baseline) -> f64 {
    // Rot = heavy hidden reasoning + repetitive output. We compute the same
    // joint metric as the baseline so z-scores make sense.
    let rot: Vec<f64> = a
        .records
        .iter()
        .filter(|r| r.out > 0 && r.th_ch > 0 && r.lex_div > 0.0)
        .map(|r| (r.th_ch as f64 / r.out as f64) * (1.0 - r.lex_div.clamp(0.0, 1.0)))
        .collect();
    if rot.is_empty() || baseline.thinking_rot_med < 1e-9 {
        return 50.0; // no thinking to judge
    }
    let cur = median(&rot);
    let mad = baseline
        .thinking_rot_mad
        .max(0.05 * baseline.thinking_rot_med);
    let z = robust_z(cur, baseline.thinking_rot_med, mad);
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
    let thinking_rot = bot_thinking_rot(a, baseline);
    let composite = brevity * 0.25
        + stalling * 0.20
        + wandering * 0.20
        + cache_drag * 0.10
        + thinking_rot * 0.25;
    let shrunk = shrink(composite, confidence(a.n));
    shrunk.round().clamp(0.0, 100.0) as u32
}

pub fn score_breakdown(a: &Aggregate, baseline: &Baseline) -> ScoreBreakdown {
    let conf = confidence(a.n);

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
    let b_thinking_rot = bot_thinking_rot(a, baseline);
    let b_raw = b_brevity * 0.25
        + b_stalling * 0.20
        + b_wandering * 0.20
        + b_cache_drag * 0.10
        + b_thinking_rot * 0.25;

    ScoreBreakdown {
        n: a.n,
        confidence: conf,
        d_total_u_ch: total_u_ch,
        d_with_u_ch: with_u_ch,
        d_chars_per_min: cur_cpm,
        d_baseline_cpm: baseline.user_chars_per_min_mean,
        d_baseline_mad: used_std,
        d_z: driver_z,
        d_raw,
        d_shrunk,
        b_brevity,
        b_stalling,
        b_wandering,
        b_cache_drag,
        b_thinking_rot,
        b_raw,
        b_shrunk: shrink(b_raw, conf),
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
    pub b_thinking_rot: f64,
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
    pub phrase: String,
    /// Short human-readable representation of the dominant ratio's value.
    pub value: String,
    /// Dominant z-score (signed).
    pub z: f64,
}

pub fn sigma_bucket(z: f64) -> u8 {
    let a = z.abs();
    if a < 1.0 {
        1
    } else if a < 1.5 {
        2
    } else if a < 2.0 {
        3
    } else if a < 3.0 {
        4
    } else {
        5
    }
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
        return Signal {
            phrase: "steady".into(),
            value: "—".into(),
            z: 0.0,
        };
    }

    let inv_cur = if out_sum > 0 {
        Some(cc_sum as f64 / out_sum as f64)
    } else {
        None
    };
    let amp_cur = if u_ch_sum > 0 {
        Some(cc_sum as f64 / u_ch_sum as f64)
    } else {
        None
    };
    let thr_cur = Some(cc_sum as f64 / span_min);

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
                "amplified",
                "manual",
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
                "calm",
                "surge",
                format!("{:.0}/m", c),
            ));
        }
    }

    if candidates.is_empty() {
        return Signal {
            phrase: "warming up".into(),
            value: "—".into(),
            z: 0.0,
        };
    }

    let mut best_z: Option<f64> = None;
    let mut best_label = "steady";
    let mut best_val = String::new();
    for (cur, med, mad, floor, low, high, fmt) in &candidates {
        let denom = mad.max(*floor);
        let z = if denom > 1e-9 {
            (cur - med) / denom
        } else {
            0.0
        };
        if best_z.map(|bz| z.abs() > bz.abs()).unwrap_or(true) {
            best_z = Some(z);
            let phrase = if z.abs() < 1.0 {
                "steady"
            } else if z > 0.0 {
                high
            } else {
                low
            };
            best_label = phrase;
            best_val = fmt.clone();
        }
    }
    let z = best_z.unwrap();
    Signal {
        phrase: format!("⟳{} {}", sigma_bucket(z), best_label),
        value: if best_val.is_empty() {
            "—".into()
        } else {
            best_val
        },
        z,
    }
}

pub fn vibe_label(score: u32) -> &'static str {
    match score {
        s if s < 20 => "crisp 🍪",
        s if s < 40 => "steady ✨",
        s if s < 50 => "on rails 💫",
        s if s < 70 => "mid 🧽",
        s if s < 90 => "cooked 🔥",
        _ => "fried 💀",
    }
}

pub fn split_summary(drv_pct: f64, bot_pct: f64, drv_n: u64, bot_n: u64) -> &'static str {
    if drv_n == 0 {
        "no driver turns observed"
    } else if bot_n == 0 {
        "pure prompting: no tool loops"
    } else if drv_pct >= 80.0 {
        "driver-heavy: lots of driving, agent doing little tool work"
    } else if bot_pct >= 80.0 {
        "bot-heavy: agent grinding through tool loops"
    } else if drv_pct >= 60.0 {
        "driver-leaning: driving more than the agent is iterating"
    } else if bot_pct >= 60.0 {
        "bot-leaning: agent doing more tool work than you're typing"
    } else {
        "balanced: driver steers, agent acts"
    }
}

pub fn diagnosis(bd: &ScoreBreakdown) -> Option<&'static str> {
    let bot = bd.b_shrunk as u32;
    let drv = bd.d_shrunk as u32;

    if bot < 30 && drv < 30 {
        return None;
    }

    // ── Driver leading ────────────────────────────────────────────────
    if bd.d_shrunk > bd.b_shrunk {
        // How bad is the driver?
        if drv > 80 {
            return Some("driver is spiraling, bot is still sharp");
        }
        if drv > 70 {
            return Some("driver over the limit, bot holds steady. slow down.");
        }
        // What is pushing the driver score up?
        let d_z = bd.d_z;
        if d_z > 1.5 {
            return Some("driving faster than your baseline, watch for burnout");
        }
        if d_z < -0.5 {
            return Some("driving slower than usual, or context is bloating");
        }
        return Some("driver pushing past normal pace");
    }

    // ── Bot leading ───────────────────────────────────────────────────
    // Find which component is the worst (lowest score = most rot)
    let components = [
        ("brevity", bd.b_brevity),
        ("stalling", bd.b_stalling),
        ("wandering", bd.b_wandering),
        ("cache_drag", bd.b_cache_drag),
        ("thinking_rot", bd.b_thinking_rot),
    ];
    let worst = components
        .iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap();

    // Bot is clearly worse — what is the problem?
    if worst.0 == "thinking_rot" {
        return Some("bot is spinning wheels, thinking without delivering");
    }
    if worst.0 == "stalling" {
        return Some("bot is slower than baseline, streaming sluggish");
    }
    if worst.0 == "brevity" {
        return Some("bot is short-circuiting, output cuts off too soon");
    }
    if worst.0 == "cache_drag" {
        return Some("cache is cold, bot reloads its context every turn");
    }
    if worst.0 == "wandering" {
        return Some("output is unstable, bot loses thread");
    }

    // Fallback: composite-only, in case no single component dominates
    let diff = (bd.b_shrunk - bd.d_shrunk).abs();
    let avg = (bd.b_shrunk + bd.d_shrunk) / 2.0;
    if diff < 15.0 {
        if avg > 60.0 {
            return Some("both sides sinking, loop has taken hold");
        }
        if avg > 40.0 {
            return Some("drift on both sides, staying alert");
        }
        return None;
    }
    if bd.b_shrunk > 70.0 {
        return Some("bot is cooked. swap it out or wipe context.");
    }
    if bd.b_shrunk > 50.0 {
        return Some("bot is stressed, output may degrading soon");
    }
    Some("bot is quiet, you are in control")
}

/// Convenience wrapper: build a ScoreBreakdown, then run diagnosis on it.
pub fn diagnosis_for(a: &Aggregate, baseline: &Baseline) -> Option<&'static str> {
    diagnosis(&score_breakdown(a, baseline))
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
            th_ch: 0,
            reference: None,
            lex_div: 0.0,
            fn_word_frac: 0.0,
            ngram_entropy: 0.0,
            nvt: 0.0,
        }
    }

    fn rec_lex(
        ts: f64,
        sid: &str,
        u_ch: u64,
        lex_div: f64,
        fnw: f64,
        nge: f64,
        nvt: f64,
    ) -> Record {
        let mut r = rec(ts, sid);
        r.u_ch = u_ch;
        r.lex_div = lex_div;
        r.fn_word_frac = fnw;
        r.ngram_entropy = nge;
        r.nvt = nvt;
        r
    }

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
        assert_eq!(
            bot_count(&det),
            bot_count(&prob),
            "clean case: counts should match"
        );
        assert!(mix.mu_bot.exp() < 5.0, "bot median should be ~1-2s");
        assert!(
            mix.mu_drv.exp() > 20.0,
            "driver median should be tens of seconds"
        );
        assert!(
            mix.mu_bot.exp() < mix.threshold_sec && mix.threshold_sec < mix.mu_drv.exp(),
            "learned boundary should sit between the two modes"
        );
        assert_eq!(prob[0], TurnKind::Driver, "first turn is always Driver");
    }

    #[test]
    fn slow_tool_loops_fix_deterministic_mislabel() {
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
        assert!(
            bot_count(&prob) > 0,
            "probabilistic should recover the bot cluster"
        );
        assert!(
            mix.threshold_sec > 5.0 && mix.threshold_sec < 60.0,
            "learned boundary should sit between the 8s and 60s modes, got {}",
            mix.threshold_sec
        );
        assert!((mix.mu_bot.exp() - 8.0).abs() < 3.0, "bot median ≈ 8s");
    }

    #[test]
    fn degenerate_data_falls_back_to_deterministic() {
        let recs = session(&[
            (3.0, TurnKind::Bot),
            (3.0, TurnKind::Bot),
            (3.0, TurnKind::Bot),
            (3.0, TurnKind::Bot),
        ]);
        let det = classify_turns(&recs);
        let (prob, mix) = classify_turns_prob_with_model(&recs);
        assert!(mix.is_none(), "degenerate data should refuse the fit");
        assert_eq!(
            bot_count(&det),
            bot_count(&prob),
            "fallback = deterministic"
        );
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
        let mut recs = vec![rec(0.0, "s1")];
        let mut ts = 0.0;
        for _ in 0..3 {
            ts += 12.0;
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
        let mut recs = vec![rec(0.0, "s1")];
        let mut ts = 0.0;
        for _ in 0..3 {
            ts += 1.5;
            recs.push(rec_lex(ts, "s1", 120, 0.85, 0.60, 4.2, 0.0));
        }
        let det = classify_turns(&recs);
        let (prob, _mix) = classify_turns_prob_with_model(&recs);
        assert_eq!(bot_count(&det), 3, "gap-only calls a fast typist a bot");
        assert_eq!(
            bot_count(&prob),
            0,
            "wordology keeps a fast typist a driver"
        );
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
        assert_eq!(
            bot_count(&det),
            0,
            "gap-only sees slow tool loops as driver"
        );
        assert_eq!(bot_count(&prob), 3, "tr_ch forces bot regardless of pacing");
    }

    #[test]
    fn wordology_cross_turn_template_reuse_rescues_slow_bot() {
        let mut recs = vec![rec(0.0, "s1")];
        let mut ts = 0.0;
        for _ in 0..3 {
            ts += 12.0;
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
        let p_small = posterior_bot(1.0, 0.5, 0.6, 0.7, 3.5, 0.9, 0.3);
        let p_mid = posterior_bot(20.0, 0.5, 0.6, 0.7, 3.5, 0.9, 0.3);
        let p_big = posterior_bot(200.0, 0.5, 0.6, 0.7, 3.5, 0.9, 0.3);
        assert!(
            (0.0..=1.0).contains(&p_small)
                && (0.0..=1.0).contains(&p_mid)
                && (0.0..=1.0).contains(&p_big)
        );
        assert!(
            p_small > p_mid && p_mid > p_big,
            "short gap ⇒ more likely bot"
        );
    }
}
