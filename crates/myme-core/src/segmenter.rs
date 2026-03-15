//! Minimum-cost path segmentation of kana input.
//!
//! Splits a kana reading into conversion segments using dictionary matches.
//! Each segment carries its reading, the list of dictionary candidates, and
//! the currently selected candidate index.
//!
//! The default algorithm is Viterbi (minimum-cost path), which considers all
//! possible segmentations and picks the globally optimal one.  A greedy
//! longest-match fallback is available via [`segment_greedy`].

use crate::candidate::{Candidate, CandidateSource};
use crate::dictionary::DictionaryLookup;

// ---------------------------------------------------------------------------
// Kana conversion helpers
// ---------------------------------------------------------------------------

/// Converts a hiragana string to katakana by shifting each character's
/// Unicode code point.  Hiragana U+3041..=U+3096 maps to katakana
/// U+30A1..=U+30F6.  Characters outside the hiragana range are passed through
/// unchanged.
fn to_katakana(hiragana: &str) -> String {
    hiragana
        .chars()
        .map(|c| {
            let cp = c as u32;
            if (0x3041..=0x3096).contains(&cp) {
                // SAFETY: the shifted value is always a valid Unicode scalar.
                char::from_u32(cp + 0x60).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

/// Returns `true` if every character in `s` is in the hiragana Unicode block
/// (U+3041..=U+3096).
fn is_all_hiragana(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| (0x3041u32..=0x3096u32).contains(&(c as u32)))
}

/// Appends katakana and hiragana fallback candidates to `seg` when the
/// reading is entirely hiragana.
///
/// * Katakana candidate — surface is the katakana form of the reading, score 1.
///   Only added when a candidate with that surface does not already exist.
/// * Hiragana candidate — surface equals the reading, score 0.
///   Only added when dictionary candidates are present (i.e. this is not
///   already a bare hiragana/unknown segment) **and** no existing candidate
///   already has that surface.
fn ensure_fallback_candidates(seg: &mut Segment) {
    if !is_all_hiragana(&seg.reading) {
        return;
    }

    let kata = to_katakana(&seg.reading);
    let already_has_kata = seg.candidates.iter().any(|c| c.surface == kata);
    if !already_has_kata {
        seg.candidates.push(Candidate::new(
            kata,
            seg.reading.clone(),
            1,
            CandidateSource::System,
        ));
    }

    // Add hiragana as-is only when there are dictionary matches (i.e. the
    // first candidate came from the dictionary, not from an unknown-char
    // fallback).  We detect this by checking whether any candidate has a
    // surface different from the reading (a sure sign of a dict entry) OR the
    // candidates list has more than one entry.
    let has_dict_matches = seg.candidates.len() > 1
        || seg
            .candidates
            .first()
            .map(|c| c.surface != seg.reading)
            .unwrap_or(false);

    if has_dict_matches {
        let already_has_hira = seg.candidates.iter().any(|c| c.surface == seg.reading);
        if !already_has_hira {
            seg.candidates.push(Candidate::new(
                seg.reading.clone(),
                seg.reading.clone(),
                0,
                CandidateSource::System,
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Segment
// ---------------------------------------------------------------------------

/// A single conversion segment produced by the segmenter.
#[derive(Debug, Clone)]
pub struct Segment {
    /// The kana reading for this segment (e.g. `"きょう"`).
    pub reading: String,
    /// Candidates from the dictionary for this reading, sorted best-first.
    /// For unknown segments this contains a single candidate whose surface
    /// equals the reading.
    pub candidates: Vec<Candidate>,
    /// Zero-based index of the currently selected candidate.
    pub selected: usize,
}

impl Segment {
    /// Returns the surface form of the currently selected candidate.
    pub fn selected_surface(&self) -> &str {
        self.candidates
            .get(self.selected)
            .map(|c| c.surface.as_str())
            .unwrap_or(&self.reading)
    }
}

// ---------------------------------------------------------------------------
// Viterbi (minimum-cost path) segmentation
// ---------------------------------------------------------------------------

/// Cost assigned to an unknown single-character fallback segment.
const UNKNOWN_COST: i64 = 80;

/// Per-segment overhead.  Each new segment incurs this fixed cost, which
/// discourages over-segmentation.
const SEGMENT_PENALTY: i64 = 8;

/// Maximum score contribution per segment.  Caps `sqrt(top_score)` so that
/// single-char entries with many candidates (high position-based scores) don't
/// dominate over compound words with fewer candidates.
const MAX_SCORE_CONTRIBUTION: i64 = 6;

/// A node in the DP lattice for Viterbi segmentation.
#[derive(Clone)]
struct LatticeNode {
    /// Accumulated cost from position 0 to this position.
    cost: i64,
    /// Reading length (in chars) of the segment that led to this node.
    /// Zero means "no path reaches here yet".
    best_prev_len: usize,
}

/// Segments a kana reading into conversion units.
///
/// Dispatches to [`segment_viterbi`] which finds the globally optimal
/// segmentation using dynamic programming.  Frequency-annotated dictionary
/// scores ensure the cost function correctly prefers common compound words
/// over single-character splits.
pub fn segment(reading: &str, dict: &dyn DictionaryLookup) -> Vec<Segment> {
    segment_viterbi(reading, dict)
}

/// Viterbi segmentation: finds the globally optimal segmentation by scoring
/// the entire path using dynamic programming.
///
/// At each position, all dictionary prefix matches are considered.  The cost
/// of a matched segment is `-(top_candidate_score)` so that high-scoring
/// candidates produce low (preferred) costs.  Unknown single-char fallbacks
/// incur a heavy penalty ([`UNKNOWN_COST`]).
pub fn segment_viterbi(reading: &str, dict: &dyn DictionaryLookup) -> Vec<Segment> {
    if reading.is_empty() {
        return Vec::new();
    }

    let chars: Vec<char> = reading.chars().collect();
    let n = chars.len();

    // DP table: dp[i] = best way to reach position i.
    let mut dp: Vec<LatticeNode> = vec![
        LatticeNode {
            cost: i64::MAX,
            best_prev_len: 0,
        };
        n + 1
    ];
    dp[0].cost = 0;

    // Forward pass: fill the DP table.
    for i in 0..n {
        if dp[i].cost == i64::MAX {
            continue; // unreachable position
        }

        let remaining: String = chars[i..].iter().collect();
        let prefix_matches = dict.common_prefix_search(&remaining);

        for (matched_reading, candidates) in &prefix_matches {
            let matched_len = matched_reading.chars().count();
            let top_score = candidates.first().map(|c| c.score as f64).unwrap_or(0.0);
            // Cost = per-segment penalty minus compressed score.
            // sqrt compresses the huge score gap between single-char entries
            // (many candidates → high score) and compound words (fewer
            // candidates → lower score), preventing single-char splits from
            // dominating.
            let score_contribution = (top_score.sqrt() as i64).min(MAX_SCORE_CONTRIBUTION);
            let seg_cost = SEGMENT_PENALTY - score_contribution;
            let new_cost = dp[i].cost + seg_cost;

            let j = i + matched_len;
            if j <= n && new_cost < dp[j].cost {
                dp[j].cost = new_cost;
                dp[j].best_prev_len = matched_len;
            }
        }

        // Unknown single-char fallback: always available.
        let new_cost = dp[i].cost + UNKNOWN_COST;
        if new_cost < dp[i + 1].cost {
            dp[i + 1].cost = new_cost;
            dp[i + 1].best_prev_len = 1;
        }
    }

    // Backtrack from position n to recover the optimal path.
    let mut seg_ranges: Vec<(usize, usize)> = Vec::new(); // (start, len_in_chars)
    let mut pos = n;
    while pos > 0 {
        let len = dp[pos].best_prev_len;
        assert!(len > 0, "Viterbi backtrack failed at position {pos}");
        seg_ranges.push((pos - len, len));
        pos -= len;
    }
    seg_ranges.reverse();

    // Build Segment objects for each range.
    let mut segments = Vec::new();
    for (start, len) in seg_ranges {
        let seg_reading: String = chars[start..start + len].iter().collect();
        let candidates = dict.lookup(&seg_reading);

        if candidates.is_empty() {
            // Unknown segment — use the reading itself as the sole candidate.
            let candidate = Candidate::new(
                seg_reading.clone(),
                seg_reading.clone(),
                0,
                CandidateSource::System,
            );
            segments.push(Segment {
                reading: seg_reading,
                candidates: vec![candidate],
                selected: 0,
            });
        } else {
            segments.push(Segment {
                reading: seg_reading,
                candidates,
                selected: 0,
            });
        }
    }

    for seg in &mut segments {
        ensure_fallback_candidates(seg);
    }

    segments
}

/// Segments a kana reading into conversion units using greedy longest-match.
///
/// At each position the algorithm finds the longest prefix with a dictionary
/// hit.  If no match is found, a single character is emitted as an "unknown"
/// segment whose sole candidate is the character itself.
///
/// Returns an empty `Vec` for an empty input string.
pub fn segment_greedy(reading: &str, dict: &dyn DictionaryLookup) -> Vec<Segment> {
    if reading.is_empty() {
        return Vec::new();
    }

    let chars: Vec<char> = reading.chars().collect();
    let mut segments = Vec::new();
    let mut pos = 0;

    while pos < chars.len() {
        // Gather the remaining text from this position.
        let remaining: String = chars[pos..].iter().collect();

        // Find all prefix matches; they come back longest-first.
        let prefix_matches = dict.common_prefix_search(&remaining);

        if let Some((matched_reading, candidates)) = prefix_matches.into_iter().next() {
            // Use the longest match.
            let matched_chars = matched_reading.chars().count();
            segments.push(Segment {
                reading: matched_reading,
                candidates,
                selected: 0,
            });
            pos += matched_chars;
        } else {
            // No dictionary match — emit the single character as-is.
            let ch: String = chars[pos].to_string();
            let candidate = Candidate::new(
                ch.clone(),
                ch.clone(),
                0,
                CandidateSource::System,
            );
            segments.push(Segment {
                reading: ch,
                candidates: vec![candidate],
                selected: 0,
            });
            pos += 1;
        }
    }

    for seg in &mut segments {
        ensure_fallback_candidates(seg);
    }

    segments
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{Candidate, CandidateSource};
    use crate::dictionary::DictionaryLookup;

    /// Mock dictionary with a few entries for testing segmentation.
    struct SegmentDict;

    impl DictionaryLookup for SegmentDict {
        fn lookup(&self, reading: &str) -> Vec<Candidate> {
            match reading {
                "きょう" => vec![
                    Candidate::new("今日", "きょう", 30, CandidateSource::System),
                    Candidate::new("京", "きょう", 20, CandidateSource::System),
                ],
                "き" => vec![
                    Candidate::new("木", "き", 10, CandidateSource::System),
                ],
                "は" => vec![
                    Candidate::new("は", "は", 10, CandidateSource::System),
                ],
                "いい" => vec![
                    Candidate::new("いい", "いい", 10, CandidateSource::System),
                    Candidate::new("良い", "いい", 5, CandidateSource::System),
                ],
                "てんき" => vec![
                    Candidate::new("天気", "てんき", 20, CandidateSource::System),
                ],
                "です" => vec![
                    Candidate::new("です", "です", 10, CandidateSource::System),
                ],
                "てん" => vec![
                    Candidate::new("天", "てん", 10, CandidateSource::System),
                ],
                _ => vec![],
            }
        }
    }

    #[test]
    fn single_word_segment() {
        let dict = SegmentDict;
        let segs = segment("きょう", &dict);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].reading, "きょう");
        assert_eq!(segs[0].candidates[0].surface, "今日");
    }

    #[test]
    fn two_word_split() {
        let dict = SegmentDict;
        let segs = segment("きょうは", &dict);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].reading, "きょう");
        assert_eq!(segs[1].reading, "は");
    }

    #[test]
    fn unknown_chars_become_single_segments() {
        let dict = SegmentDict;
        let segs = segment("xyz", &dict);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].reading, "x");
        assert_eq!(segs[0].candidates[0].surface, "x");
        assert_eq!(segs[1].reading, "y");
        assert_eq!(segs[2].reading, "z");
    }

    #[test]
    fn empty_input_returns_empty() {
        let dict = SegmentDict;
        let segs = segment("", &dict);
        assert!(segs.is_empty());
    }

    #[test]
    fn greedy_prefers_longer_match() {
        // "きょう" should be matched as one segment, not "き" + "ょう"
        let dict = SegmentDict;
        let segs = segment("きょう", &dict);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].reading, "きょう");
    }

    #[test]
    fn multi_segment_sentence() {
        let dict = SegmentDict;
        let segs = segment("きょうはいいてんきです", &dict);
        let readings: Vec<&str> = segs.iter().map(|s| s.reading.as_str()).collect();
        assert_eq!(readings, vec!["きょう", "は", "いい", "てんき", "です"]);
    }

    #[test]
    fn selected_surface_returns_first_candidate() {
        let dict = SegmentDict;
        let segs = segment("きょう", &dict);
        assert_eq!(segs[0].selected_surface(), "今日");
    }

    #[test]
    fn mixed_known_unknown() {
        let dict = SegmentDict;
        let segs = segment("きょうxは", &dict);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].reading, "きょう");
        assert_eq!(segs[1].reading, "x");
        assert_eq!(segs[2].reading, "は");
    }

    // -----------------------------------------------------------------------
    // Viterbi-specific tests
    // -----------------------------------------------------------------------

    /// A mock dictionary where greedy longest-match makes a suboptimal choice.
    ///
    /// Uses realistic score magnitudes: common words like "きょう" have many
    /// candidates (high score), while cross-boundary matches like "きょうは"
    /// have few (low score).  Greedy picks the longer "きょうは" but Viterbi
    /// should prefer the shorter split because the combined quality is higher.
    struct ViterbiDict;

    impl DictionaryLookup for ViterbiDict {
        fn lookup(&self, reading: &str) -> Vec<Candidate> {
            match reading {
                "きょうは" => vec![
                    Candidate::new("教派", "きょうは", 10, CandidateSource::System),
                ],
                "きょう" => vec![
                    Candidate::new("今日", "きょう", 100, CandidateSource::System),
                    Candidate::new("京", "きょう", 90, CandidateSource::System),
                ],
                "は" => vec![
                    Candidate::new("は", "は", 40, CandidateSource::System),
                    Candidate::new("葉", "は", 30, CandidateSource::System),
                ],
                _ => vec![],
            }
        }
    }

    #[test]
    fn viterbi_beats_greedy_on_suboptimal_long_match() {
        let dict = ViterbiDict;

        // Greedy picks the longest match: "きょうは" → "教派" (score 10)
        let greedy = segment_greedy("きょうは", &dict);
        assert_eq!(greedy.len(), 1);
        assert_eq!(greedy[0].reading, "きょうは");

        // Viterbi picks the better overall split: "きょう" + "は"
        // sqrt(100)=10, sqrt(40)≈6 → total cost ≈ (10-10)+(10-6) = 4
        // vs "きょうは" sqrt(10)≈3 → cost = 10-3 = 7
        let viterbi = segment_viterbi("きょうは", &dict);
        assert_eq!(viterbi.len(), 2);
        assert_eq!(viterbi[0].reading, "きょう");
        assert_eq!(viterbi[0].candidates[0].surface, "今日");
        assert_eq!(viterbi[1].reading, "は");
    }

    #[test]
    fn viterbi_greedy_match_on_simple_cases() {
        // For cases where greedy and Viterbi should agree, verify both produce
        // the same segmentation.
        let dict = SegmentDict;

        let greedy = segment_greedy("きょうはいいてんきです", &dict);
        let viterbi = segment_viterbi("きょうはいいてんきです", &dict);

        let greedy_readings: Vec<&str> = greedy.iter().map(|s| s.reading.as_str()).collect();
        let viterbi_readings: Vec<&str> = viterbi.iter().map(|s| s.reading.as_str()).collect();
        assert_eq!(greedy_readings, viterbi_readings);
    }

    // -----------------------------------------------------------------------
    // Fallback candidate tests
    // -----------------------------------------------------------------------

    #[test]
    fn to_katakana_conversion() {
        assert_eq!(to_katakana("きょう"), "キョウ");
        assert_eq!(to_katakana("あいうえお"), "アイウエオ");
        // Non-hiragana characters pass through unchanged.
        assert_eq!(to_katakana("abc"), "abc");
        // Mixed input.
        assert_eq!(to_katakana("きaう"), "キaウ");
    }

    #[test]
    fn katakana_candidate_is_added() {
        // "きょう" has dictionary matches; the fallback must append a katakana
        // candidate "キョウ" at the end of the list.
        let dict = SegmentDict;
        let segs = segment("きょう", &dict);
        let surfaces: Vec<&str> = segs[0].candidates.iter().map(|c| c.surface.as_str()).collect();
        assert!(
            surfaces.contains(&"キョウ"),
            "expected キョウ in candidates, got {surfaces:?}"
        );
        // Katakana candidate should be near the end (low score).
        let kata_pos = surfaces.iter().position(|&s| s == "キョウ").unwrap();
        assert!(
            kata_pos >= 2,
            "katakana candidate should be after dict candidates, got pos {kata_pos}"
        );
    }

    #[test]
    fn hiragana_candidate_is_added() {
        // "きょう" has dictionary matches; the fallback must also append a
        // hiragana as-is candidate "きょう".
        let dict = SegmentDict;
        let segs = segment("きょう", &dict);
        let surfaces: Vec<&str> = segs[0].candidates.iter().map(|c| c.surface.as_str()).collect();
        assert!(
            surfaces.contains(&"きょう"),
            "expected きょう in candidates, got {surfaces:?}"
        );
    }

    #[test]
    fn katakana_not_duplicated() {
        // Build a dict that already returns a katakana surface for a reading.
        struct KataDict;
        impl DictionaryLookup for KataDict {
            fn lookup(&self, reading: &str) -> Vec<Candidate> {
                match reading {
                    "き" => vec![
                        Candidate::new("キ", "き", 10, CandidateSource::System),
                    ],
                    _ => vec![],
                }
            }
        }

        let dict = KataDict;
        let segs = segment_greedy("き", &dict);
        let kata_count = segs[0]
            .candidates
            .iter()
            .filter(|c| c.surface == "キ")
            .count();
        assert_eq!(kata_count, 1, "katakana candidate should not be duplicated");
    }

    #[test]
    fn unknown_segment_gets_katakana() {
        // "ん" has no dictionary entry; it becomes an unknown segment.  After
        // Fix 1 it should gain a katakana candidate "ン".
        let dict = SegmentDict;
        let segs = segment("ん", &dict);
        assert_eq!(segs.len(), 1);
        let surfaces: Vec<&str> = segs[0].candidates.iter().map(|c| c.surface.as_str()).collect();
        assert!(
            surfaces.contains(&"ン"),
            "expected ン in candidates for unknown ん, got {surfaces:?}"
        );
    }
}
