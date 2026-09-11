//! Pure text fingerprinting: lexical stats, bigrams, novelty, system-block
//! stripping. Split out of the `ccft` binary's request handler so the text
//! metrics are usable standalone — they're plain `&str -> numbers/strings`,
//! with zero I/O.

use std::collections::{HashMap, HashSet};

/// Common function words excluded from content-bigram novelty.
pub const FUNCTION_WORDS: &[&str] = &[
    "the", "a", "an", "of", "in", "on", "at", "to", "for", "with", "from", "by", "and", "or",
    "but", "not", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had", "do",
    "does", "did", "will", "would", "can", "could", "should", "may", "might", "must", "i", "you",
    "he", "she", "it", "we", "they", "me", "my", "your", "his", "her", "our", "their", "this",
    "that", "these", "those", "there", "here", "then", "now", "as", "if", "than", "so", "about",
    "into", "after", "before", "between", "over", "under", "again", "once", "off", "up", "down",
    "out", "very", "just", "more", "most", "less", "least", "no", "yes", "what", "which", "when",
    "where", "how", "who",
];

/// Lexical stats over a text: returns `(ttr, fn_word_frac, bigram_entropy)`.
///
/// - `ttr`: type/token ratio (lexical diversity), 0 when text too short (<8 tokens).
/// - `fn_word_frac`: fraction of tokens that are function words.
/// - `bigram_entropy`: Shannon entropy over adjacent-token bigram counts.
///
/// All three are 0.0 when the text has fewer than 8 tokens.
pub fn lexical_stats(text: &str) -> (f64, f64, f64) {
    let lower = text.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let n = tokens.len();
    if n < 8 {
        return (0.0, 0.0, 0.0);
    }
    let mut uniq: HashSet<&str> = HashSet::new();
    uniq.extend(tokens.iter().copied());
    let ttr = uniq.len() as f64 / n as f64;
    let fnw = tokens.iter().filter(|t| FUNCTION_WORDS.contains(t)).count() as f64 / n as f64;
    let mut counts: HashMap<(String, String), f64> = HashMap::new();
    for w in tokens.windows(2) {
        *counts
            .entry((w[0].to_string(), w[1].to_string()))
            .or_default() += 1.0;
    }
    let total: f64 = counts.values().sum();
    let nge = if total > 0.0 {
        counts
            .values()
            .map(|c| {
                let p = c / total;
                -p * p.log2()
            })
            .sum()
    } else {
        0.0
    };
    (ttr, fnw, nge)
}

/// Content bigrams of a text: adjacent token pairs where at least one token
/// is *not* a function word. These are the template-reuse signals.
pub fn content_bigrams(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let mut out = Vec::new();
    for w in tokens.windows(2) {
        if !FUNCTION_WORDS.contains(&w[0]) || !FUNCTION_WORDS.contains(&w[1]) {
            out.push(format!("{} {}", w[0], w[1]));
        }
    }
    out
}

/// Novelty of a text against a set of already-seen content bigrams.
///
/// Returns `(seen_fraction, to_merge)`:
/// - `seen_fraction` — fraction of this text's content bigrams already in `seen`
///   (higher = more repetitive/templated).
/// - `to_merge` — the bigrams not yet in `seen` (caller can merge them in).
///
/// When `seen` is `None`, every bigram counts as new (`seen_fraction` = 0.0).
pub fn novelty_fraction(text: &str, seen: Option<&HashSet<String>>) -> (f64, Vec<String>) {
    let bigs = content_bigrams(text);
    if bigs.is_empty() {
        return (0.0, Vec::new());
    }
    let total = bigs.len() as f64;
    let mut seen_count = 0.0f64;
    let mut to_merge = Vec::new();
    match seen {
        Some(s) => {
            for b in &bigs {
                if s.contains(b) {
                    seen_count += 1.0;
                } else {
                    to_merge.push(b.clone());
                }
            }
        }
        None => {
            to_merge.reserve(bigs.len());
            for b in bigs {
                to_merge.push(b);
            }
        }
    }
    (seen_count / total, to_merge)
}

/// Strip `<system-...>...</system-...>` blocks from a string.
///
/// Handles arbitrary `<system-*>` tag names (e.g. `<system-reminder>`) and
/// their matching close tag, leaving all other text untouched. If a tag is
/// unclosed, the rest of the string is kept verbatim.
pub fn strip_system_blocks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let Some(open_at) = rest.find("<system-") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..open_at]);
        let after_open = &rest[open_at + 1..]; // strip the `<`
        let Some(name_end) = after_open.find('>') else {
            out.push_str(&rest[open_at..]);
            break;
        };
        let tag_name = &after_open[..name_end]; // e.g. "system-reminder"
        let close_pat = format!("</{}>", tag_name);
        let after_tag = &after_open[name_end + 1..];
        match after_tag.find(&close_pat) {
            Some(close_at) => {
                rest = &after_tag[close_at + close_pat.len()..];
            }
            None => {
                out.push_str(&rest[open_at..]);
                break;
            }
        }
    }
    out
}

/// Clean user text for fingerprinting: drop the "continued from previous
/// conversation" preamble and strip system blocks.
pub fn clean_user_text(s: &str) -> String {
    if s.trim_start()
        .starts_with("This session is being continued from a previous conversation")
    {
        return String::new();
    }
    strip_system_blocks(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_stats_too_short_is_zero() {
        assert_eq!(lexical_stats("hi there"), (0.0, 0.0, 0.0));
    }

    #[test]
    fn lexical_stats_direction() {
        let (t1, _, _) = lexical_stats(&"a b c d e f g h i j k l m n o p q r s t".repeat(20));
        let (t2, _, _) = lexical_stats(&"the the the the the the the the the the".repeat(20));
        assert!(t1 > t2, "diverse text should have higher TTR");
    }

    #[test]
    fn content_bigrams_skip_function_word_glue() {
        // Pure function-word glue has no content bigrams.
        let bigs = content_bigrams("of the and to be a in for it");
        assert!(bigs.is_empty(), "glue-only text has no content bigrams");
        // A bigram with at least one content word is kept (here: "quick").
        let bigs = content_bigrams("the quick brown fox jumps");
        assert!(bigs.contains(&"quick brown".to_string()));
    }

    #[test]
    fn novelty_tracks_template_reuse() {
        let mut seen = HashSet::new();
        let (f1, merge) = novelty_fraction("the quick brown fox", Some(&seen));
        assert_eq!(f1, 0.0);
        seen.extend(merge);
        let (f2, _) = novelty_fraction("the quick brown fox", Some(&seen));
        assert!(f2 > 0.0);
    }

    #[test]
    fn strip_system_blocks_removes_blocks() {
        let out = strip_system_blocks("hi <system-reminder>secret</system-reminder> bye");
        assert_eq!(out, "hi  bye");
    }
}
