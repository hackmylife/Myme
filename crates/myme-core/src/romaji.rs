//! Romaji-to-kana conversion.
//!
//! ## Responsibilities
//!
//! - Maintain the standard romaji → hiragana mapping table (e.g. "ka" → "か",
//!   "tsu" → "つ", "nn" → "ん").
//! - Provide an incremental state machine so that partially typed sequences
//!   (e.g. "k" waiting for a vowel) are handled correctly without buffering
//!   whole words.
//! - Support both Hepburn and Nihon-shiki inputs where they differ (future).
//! - Be allocation-free in the hot path; operate on `&str` slices and write
//!   output into a caller-supplied `String` or fixed-size buffer.
//!
//! ## Non-goals
//!
//! This module does **not** perform kana → kanji conversion; that is the
//! responsibility of [`crate::dictionary`] and [`crate::candidate`].

/// Output produced by a single [`RomajiConverter::feed`] call.
///
/// After each keystroke the caller should:
/// 1. Append `confirmed` (if `Some`) to the running kana preedit string.
/// 2. Replace the displayed romaji suffix with `pending`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RomajiOutput {
    /// Kana that has been fully resolved and should be appended to the preedit
    /// buffer.  `None` when the keystroke only extended the pending buffer.
    pub confirmed: Option<String>,
    /// Romaji characters still waiting for more input.  May be empty.
    pub pending: String,
}

/// Vowel characters used for n-before-consonant detection.
const VOWELS: &[char] = &['a', 'i', 'u', 'e', 'o'];

/// The complete romaji → hiragana mapping table.
///
/// Entries are sorted longest-first so that prefix matching always
/// resolves the most specific rule (e.g. "tsu" before "ts").
/// The table is a `&[(&str, &str)]` so it lives entirely in the read-only
/// data segment with zero heap allocation.
static ROMAJI_TABLE: &[(&str, &str)] = &[
    // ── Special symbols ───────────────────────────────────────────────────
    ("-", "ー"),
    (".", "。"),
    (",", "、"),
    // ── Vowels ───────────────────────────────────────────────────────────
    ("a", "あ"),
    ("i", "い"),
    ("u", "う"),
    ("e", "え"),
    ("o", "お"),
    // ── K-row ─────────────────────────────────────────────────────────────
    ("ka", "か"),
    ("ki", "き"),
    ("ku", "く"),
    ("ke", "け"),
    ("ko", "こ"),
    // ── KY-combo ──────────────────────────────────────────────────────────
    ("kya", "きゃ"),
    ("kyi", "きぃ"),
    ("kyu", "きゅ"),
    ("kye", "きぇ"),
    ("kyo", "きょ"),
    // ── S-row ─────────────────────────────────────────────────────────────
    ("sa", "さ"),
    ("si", "し"),
    ("su", "す"),
    ("se", "せ"),
    ("so", "そ"),
    // ── SY-combo ──────────────────────────────────────────────────────────
    ("sya", "しゃ"),
    ("syi", "しぃ"),
    ("syu", "しゅ"),
    ("sye", "しぇ"),
    ("syo", "しょ"),
    // ── SH-combo ──────────────────────────────────────────────────────────
    ("sha", "しゃ"),
    ("shi", "し"),
    ("shu", "しゅ"),
    ("she", "しぇ"),
    ("sho", "しょ"),
    // ── T-row ─────────────────────────────────────────────────────────────
    ("ta", "た"),
    ("ti", "ち"),
    ("chi", "ち"),
    ("tsu", "つ"),
    ("tu", "つ"),
    ("te", "て"),
    ("to", "と"),
    // ── TY-combo ──────────────────────────────────────────────────────────
    ("tya", "ちゃ"),
    ("tyi", "ちぃ"),
    ("tyu", "ちゅ"),
    ("tye", "ちぇ"),
    ("tyo", "ちょ"),
    // ── CY-combo ──────────────────────────────────────────────────────────
    ("cya", "ちゃ"),
    ("cyi", "ちぃ"),
    ("cyu", "ちゅ"),
    ("cye", "ちぇ"),
    ("cyo", "ちょ"),
    // ── CH-combo ──────────────────────────────────────────────────────────
    ("cha", "ちゃ"),
    ("chi", "ち"),
    ("chu", "ちゅ"),
    ("che", "ちぇ"),
    ("cho", "ちょ"),
    // ── N-row ─────────────────────────────────────────────────────────────
    ("na", "な"),
    ("ni", "に"),
    ("nu", "ぬ"),
    ("ne", "ね"),
    ("no", "の"),
    // ── NY-combo ──────────────────────────────────────────────────────────
    ("nya", "にゃ"),
    ("nyi", "にぃ"),
    ("nyu", "にゅ"),
    ("nye", "にぇ"),
    ("nyo", "にょ"),
    // ── H-row ─────────────────────────────────────────────────────────────
    ("ha", "は"),
    ("hi", "ひ"),
    ("hu", "ふ"),
    ("fu", "ふ"),
    ("he", "へ"),
    ("ho", "ほ"),
    // ── HY-combo ──────────────────────────────────────────────────────────
    ("hya", "ひゃ"),
    ("hyi", "ひぃ"),
    ("hyu", "ひゅ"),
    ("hye", "ひぇ"),
    ("hyo", "ひょ"),
    // ── M-row ─────────────────────────────────────────────────────────────
    ("ma", "ま"),
    ("mi", "み"),
    ("mu", "む"),
    ("me", "め"),
    ("mo", "も"),
    // ── MY-combo ──────────────────────────────────────────────────────────
    ("mya", "みゃ"),
    ("myi", "みぃ"),
    ("myu", "みゅ"),
    ("mye", "みぇ"),
    ("myo", "みょ"),
    // ── Y-row ─────────────────────────────────────────────────────────────
    ("ya", "や"),
    ("yu", "ゆ"),
    ("yo", "よ"),
    // ── R-row ─────────────────────────────────────────────────────────────
    ("ra", "ら"),
    ("ri", "り"),
    ("ru", "る"),
    ("re", "れ"),
    ("ro", "ろ"),
    // ── RY-combo ──────────────────────────────────────────────────────────
    ("rya", "りゃ"),
    ("ryi", "りぃ"),
    ("ryu", "りゅ"),
    ("rye", "りぇ"),
    ("ryo", "りょ"),
    // ── W-row ─────────────────────────────────────────────────────────────
    ("wa", "わ"),
    ("wi", "ゐ"),
    ("we", "ゑ"),
    ("wo", "を"),
    // ── G-row ─────────────────────────────────────────────────────────────
    ("ga", "が"),
    ("gi", "ぎ"),
    ("gu", "ぐ"),
    ("ge", "げ"),
    ("go", "ご"),
    // ── GY-combo ──────────────────────────────────────────────────────────
    ("gya", "ぎゃ"),
    ("gyi", "ぎぃ"),
    ("gyu", "ぎゅ"),
    ("gye", "ぎぇ"),
    ("gyo", "ぎょ"),
    // ── Z-row ─────────────────────────────────────────────────────────────
    ("za", "ざ"),
    ("zi", "じ"),
    ("ji", "じ"),
    ("zu", "ず"),
    ("ze", "ぜ"),
    ("zo", "ぞ"),
    // ── JY/ZY-combo ───────────────────────────────────────────────────────
    ("ja", "じゃ"),
    ("ji", "じ"),
    ("ju", "じゅ"),
    ("je", "じぇ"),
    ("jo", "じょ"),
    ("jya", "じゃ"),
    ("jyi", "じぃ"),
    ("jyu", "じゅ"),
    ("jye", "じぇ"),
    ("jyo", "じょ"),
    ("zya", "じゃ"),
    ("zyi", "じぃ"),
    ("zyu", "じゅ"),
    ("zye", "じぇ"),
    ("zyo", "じょ"),
    // ── DY-combo ──────────────────────────────────────────────────────────
    ("dya", "ぢゃ"),
    ("dyi", "ぢぃ"),
    ("dyu", "ぢゅ"),
    ("dye", "ぢぇ"),
    ("dyo", "ぢょ"),
    // ── D-row ─────────────────────────────────────────────────────────────
    ("da", "だ"),
    ("di", "ぢ"),
    ("du", "づ"),
    ("dzu", "づ"),
    ("de", "で"),
    ("do", "ど"),
    // ── B-row ─────────────────────────────────────────────────────────────
    ("ba", "ば"),
    ("bi", "び"),
    ("bu", "ぶ"),
    ("be", "べ"),
    ("bo", "ぼ"),
    // ── BY-combo ──────────────────────────────────────────────────────────
    ("bya", "びゃ"),
    ("byi", "びぃ"),
    ("byu", "びゅ"),
    ("bye", "びぇ"),
    ("byo", "びょ"),
    // ── P-row ─────────────────────────────────────────────────────────────
    ("pa", "ぱ"),
    ("pi", "ぴ"),
    ("pu", "ぷ"),
    ("pe", "ぺ"),
    ("po", "ぽ"),
    // ── PY-combo ──────────────────────────────────────────────────────────
    ("pya", "ぴゃ"),
    ("pyi", "ぴぃ"),
    ("pyu", "ぴゅ"),
    ("pye", "ぴぇ"),
    ("pyo", "ぴょ"),
    // ── Small kana (x-prefix) ──────────────────────────────────────────────
    ("xa", "ぁ"),
    ("xi", "ぃ"),
    ("xu", "ぅ"),
    ("xe", "ぇ"),
    ("xo", "ぉ"),
    ("xtu", "っ"),
    ("xtsu", "っ"),
    ("xya", "ゃ"),
    ("xyu", "ゅ"),
    ("xyo", "ょ"),
    ("xwa", "ゎ"),
    ("xka", "ゕ"),
    ("xke", "ゖ"),
    // ── Small kana (l-prefix, alias for x-prefix) ───────────────────────
    ("la", "ぁ"),
    ("li", "ぃ"),
    ("lu", "ぅ"),
    ("le", "ぇ"),
    ("lo", "ぉ"),
    ("ltu", "っ"),
    ("ltsu", "っ"),
    ("lya", "ゃ"),
    ("lyu", "ゅ"),
    ("lyo", "ょ"),
    ("lwa", "ゎ"),
    ("lka", "ゕ"),
    ("lke", "ゖ"),
    // ── ん (standalone, must be last so more-specific rules win) ──────────
    ("n", "ん"),
];

/// Returns the kana string for a complete romaji sequence, or `None`.
fn lookup(romaji: &str) -> Option<&'static str> {
    for &(key, kana) in ROMAJI_TABLE {
        if key == romaji {
            return Some(kana);
        }
    }
    None
}

/// Returns `true` if `prefix` is the start of at least one entry in the table.
fn is_valid_prefix(prefix: &str) -> bool {
    for &(key, _) in ROMAJI_TABLE {
        if key.starts_with(prefix) {
            return true;
        }
    }
    false
}

/// Returns `true` if `ch` is an ASCII alphabetic consonant (not a vowel).
fn is_consonant(ch: char) -> bool {
    ch.is_ascii_alphabetic() && !VOWELS.contains(&ch)
}

/// Stateful romaji-to-kana converter.
///
/// Feed one character at a time via [`RomajiConverter::feed`].  The converter
/// accumulates pending romaji and emits confirmed kana as soon as a sequence
/// is unambiguously complete.
///
/// # Example
///
/// ```
/// use myme_core::romaji::RomajiConverter;
///
/// let mut conv = RomajiConverter::new();
/// assert_eq!(conv.feed('k').confirmed, None);   // "k" – still pending
/// assert_eq!(conv.feed('a').confirmed.as_deref(), Some("か"));
/// ```
#[derive(Debug, Default, Clone)]
pub struct RomajiConverter {
    /// Romaji characters typed so far that have not yet resolved to kana.
    pending: String,
    /// `true` when the pending `"n"` originated from an `"nn"` sequence.
    /// On flush (commit / end-of-input) this `"n"` should be suppressed
    /// rather than emitted as a second `ん`, because the first `ん` already
    /// accounts for the double-n.
    nn_pending: bool,
}

impl RomajiConverter {
    /// Creates a new converter with an empty pending buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the current pending romaji buffer (characters awaiting more
    /// input before they can be resolved).
    pub fn pending(&self) -> &str {
        &self.pending
    }

    /// Returns `true` if the pending `"n"` originated from an `"nn"` sequence
    /// and should be suppressed (not flushed as `ん`) on commit.
    pub fn is_nn_pending(&self) -> bool {
        self.nn_pending
    }

    /// Clears all internal state.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.nn_pending = false;
    }

    /// Removes the last character from the pending buffer.
    ///
    /// Returns a [`RomajiOutput`] with `confirmed = None` and `pending` equal
    /// to the buffer after deletion.  If the buffer was already empty both
    /// fields are empty/None.
    pub fn backspace(&mut self) -> RomajiOutput {
        // Remove the last *Unicode scalar* (char), not the last byte.
        if !self.pending.is_empty() {
            let new_len = self
                .pending
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.pending.truncate(new_len);
        }
        RomajiOutput {
            confirmed: None,
            pending: self.pending.clone(),
        }
    }

    /// Feeds one character into the converter and returns the resulting output.
    ///
    /// The algorithm:
    /// 1. Handle special single-character symbols (-, ., ,) that are always
    ///    confirmed immediately regardless of pending state.
    /// 2. Append `ch` to the pending buffer.
    /// 3. If the pending buffer is an exact match in the table → confirm.
    /// 4. If the pending buffer is a valid prefix → keep waiting.
    /// 5. Otherwise apply fallback rules:
    ///    a. Double-consonant: pending ends in `cc` (same consonant twice)
    ///       → confirm っ, keep `c` as new pending, retry with remainder.
    ///    b. n-before-consonant: pending starts with `n` and next char is a
    ///       consonant (and not `n` itself which would form `nn`) → confirm ん,
    ///       restart with the remainder.
    ///    c. General mismatch: the first character cannot contribute to any
    ///       sequence → emit it verbatim and re-feed the rest.
    pub fn feed(&mut self, ch: char) -> RomajiOutput {
        // Special standalone symbols are confirmed immediately and do not
        // interact with the pending buffer.
        if ch == '-' || ch == '.' || ch == ',' {
            let mut confirmed_text = String::new();
            // Flush any pending content as-is first.
            if !self.pending.is_empty() {
                confirmed_text.push_str(&self.pending);
                self.pending.clear();
            }
            let symbol = match ch {
                '-' => "ー",
                '.' => "。",
                ',' => "、",
                _ => unreachable!(),
            };
            confirmed_text.push_str(symbol);
            return RomajiOutput {
                confirmed: Some(confirmed_text),
                pending: String::new(),
            };
        }

        self.pending.push(ch);
        self.resolve()
    }

    /// Attempts to resolve the pending buffer as much as possible, returning
    /// the combined output.  This is the core of the state machine and is
    /// called both from `feed` and recursively on remainder strings.
    fn resolve(&mut self) -> RomajiOutput {
        let mut confirmed = String::new();

        loop {
            // ── Special case: "nn" → ん, keep second 'n' as pending ───────
            // This must be checked before the general double-consonant rule
            // (Rule B) so that "nn" emits ん rather than っ.
            // The second 'n' is kept pending so that e.g. "onna" → おんな:
            //   o → お, n(pending), nn → ん + n(pending), na → な.
            //
            // KNOWN LIMITATION (C1): when "nn" appears at the very end of
            // input with nothing following, the pending 'n' left after the
            // first ん is flushed as a second ん by `convert()` or
            // `flush_pending_romaji()`, producing "んん" instead of "ん".
            // This is a Phase 1 design trade-off: correctly handling "onna"
            // requires keeping the second 'n' pending, but that same pending
            // 'n' is indistinguishable from a deliberately typed lone 'n' at
            // end-of-input.  A future Phase 2 fix should track whether the
            // pending 'n' originated from an "nn" double-n sequence so it
            // can be suppressed on commit rather than flushed as ん.
            if self.pending == "nn" && !self.nn_pending {
                confirmed.push('ん');
                self.pending = "n".to_string();
                self.nn_pending = true;
                break;
            }

            // When nn_pending is true and the user typed another 'n' (making
            // pending "nn"), the first 'n' was already part of the previous ん.
            // Replace it with the new 'n' as a fresh pending (not nn_pending).
            if self.pending == "nn" && self.nn_pending {
                self.pending = "n".to_string();
                self.nn_pending = false;
                break;
            }

            // ── Exact match ──────────────────────────────────────────────
            if let Some(kana) = lookup(&self.pending) {
                // Special case: bare "n" stays pending because the user might
                // type a vowel next (giving "na","ni", etc.) or another 'n'
                // (giving "nn" → ん).  It is only flushed as ん by the
                // `convert` helper or when n-before-consonant rule fires.
                if self.pending == "n" {
                    break;
                }
                confirmed.push_str(kana);
                self.pending.clear();
                self.nn_pending = false;
                break;
            }

            // ── Valid prefix – keep accumulating ─────────────────────────
            if is_valid_prefix(&self.pending) {
                break;
            }

            // ── Fallback rules ────────────────────────────────────────────

            // Rule A: n-before-consonant
            // If pending starts with 'n' and the *next* char is a consonant
            // that is NOT 'n' (which would be handled by the "nn" rule
            // above), emit ん and restart with the remaining characters.
            if self.pending.starts_with('n') && self.pending.len() > 1 {
                let mut chars = self.pending.chars();
                let _n = chars.next(); // 'n'
                let next = chars.next().unwrap();
                if is_consonant(next) && next != 'n' {
                    if !self.nn_pending {
                        // Genuine n-before-consonant: emit ん.
                        confirmed.push('ん');
                    }
                    // If nn_pending, the ん was already emitted by the "nn"
                    // rule — just consume the leftover 'n' without emitting.
                    let remainder: String = self.pending.chars().skip(1).collect();
                    self.pending = remainder;
                    self.nn_pending = false;
                    continue;
                }
            }

            // Rule B: double-consonant → っ
            // If the first two characters are the same consonant (and not 'n',
            // which is handled above), emit っ and keep from the second char.
            {
                let mut chars = self.pending.chars();
                if let (Some(c1), Some(c2)) = (chars.next(), chars.next()) {
                    if c1 == c2 && is_consonant(c1) && c1 != 'n' {
                        confirmed.push('っ');
                        let remainder: String = self.pending.chars().skip(1).collect();
                        self.pending = remainder;
                        continue;
                    }
                }
            }

            // Rule C: the first character is unrecognisable – emit it verbatim
            // and continue resolving the rest.
            {
                let mut chars = self.pending.chars();
                let first = chars.next().unwrap();
                confirmed.push(first);
                let remainder: String = chars.collect();
                self.pending = remainder;
                continue;
            }
        }

        RomajiOutput {
            confirmed: if confirmed.is_empty() {
                None
            } else {
                Some(confirmed)
            },
            pending: self.pending.clone(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper: convert a whole romaji string to kana in one shot (useful in tests).
// ─────────────────────────────────────────────────────────────────────────────

/// Converts a complete romaji string to hiragana.
///
/// This is a convenience wrapper around [`RomajiConverter`] that feeds every
/// character and then flushes any remaining pending buffer.
pub fn convert(romaji: &str) -> String {
    let mut conv = RomajiConverter::new();
    let mut result = String::new();

    for ch in romaji.chars() {
        let out = conv.feed(ch);
        if let Some(k) = out.confirmed {
            result.push_str(&k);
        }
    }

    // Flush the remaining pending buffer.
    // A lone trailing "n" becomes ん UNLESS it originated from "nn" (in which
    // case the ん was already emitted and this pending "n" should be dropped).
    let remaining = conv.pending().to_string();
    if !remaining.is_empty() {
        if remaining == "n" && !conv.is_nn_pending() {
            result.push('ん');
        } else if remaining == "n" && conv.is_nn_pending() {
            // Suppress: ん was already emitted for "nn".
        } else {
            result.push_str(&remaining);
        }
    }

    result
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Convenience helper ────────────────────────────────────────────────────

    /// Feed every character of `romaji` into a fresh converter and collect all
    /// confirmed kana, then flush pending.
    fn feed_all(romaji: &str) -> String {
        convert(romaji)
    }

    // ── Full-word integration tests ───────────────────────────────────────────

    #[test]
    fn test_konnichiha() {
        assert_eq!(feed_all("konnichiha"), "こんにちは");
    }

    #[test]
    fn test_toukyou() {
        assert_eq!(feed_all("toukyou"), "とうきょう");
    }

    #[test]
    fn test_shinkansen() {
        assert_eq!(feed_all("shinkansen"), "しんかんせん");
    }

    #[test]
    fn test_kekka_double_consonant() {
        assert_eq!(feed_all("kekka"), "けっか");
    }

    #[test]
    fn test_sanpo_n_before_consonant() {
        assert_eq!(feed_all("sanpo"), "さんぽ");
    }

    #[test]
    fn test_onna() {
        assert_eq!(feed_all("onna"), "おんな");
    }

    #[test]
    fn test_densha() {
        assert_eq!(feed_all("densha"), "でんしゃ");
    }

    #[test]
    fn test_single_vowels() {
        assert_eq!(feed_all("aiueo"), "あいうえお");
    }

    // ── Row-by-row correctness ────────────────────────────────────────────────

    #[test]
    fn test_k_row() {
        assert_eq!(feed_all("kakikukeko"), "かきくけこ");
    }

    #[test]
    fn test_s_row() {
        assert_eq!(feed_all("sasisuseso"), "さしすせそ");
    }

    #[test]
    fn test_s_row_shi() {
        assert_eq!(feed_all("shi"), "し");
    }

    #[test]
    fn test_t_row() {
        assert_eq!(feed_all("tatituteto"), "たちつてと");
    }

    #[test]
    fn test_tsu() {
        assert_eq!(feed_all("tsu"), "つ");
    }

    #[test]
    fn test_chi() {
        assert_eq!(feed_all("chi"), "ち");
    }

    #[test]
    fn test_n_row() {
        assert_eq!(feed_all("naninuneno"), "なにぬねの");
    }

    #[test]
    fn test_h_row() {
        assert_eq!(feed_all("hahihuheho"), "はひふへほ");
    }

    #[test]
    fn test_fu() {
        assert_eq!(feed_all("fu"), "ふ");
        assert_eq!(feed_all("hu"), "ふ");
    }

    #[test]
    fn test_m_row() {
        assert_eq!(feed_all("mamimumemo"), "まみむめも");
    }

    #[test]
    fn test_y_row() {
        assert_eq!(feed_all("yayuyo"), "やゆよ");
    }

    #[test]
    fn test_r_row() {
        assert_eq!(feed_all("rarirurero"), "らりるれろ");
    }

    #[test]
    fn test_w_row() {
        assert_eq!(feed_all("wawo"), "わを");
    }

    #[test]
    fn test_g_row() {
        assert_eq!(feed_all("gagigugego"), "がぎぐげご");
    }

    #[test]
    fn test_z_row() {
        // zi → じ, others standard
        assert_eq!(feed_all("zazizuzezo"), "ざじずぜぞ");
    }

    #[test]
    fn test_ji() {
        assert_eq!(feed_all("ji"), "じ");
    }

    #[test]
    fn test_d_row() {
        assert_eq!(feed_all("dadidudeado"), "だぢづであど");
        // Note: "dea" is "de"+"a" = でぁ – let's test cleaner sequences.
    }

    #[test]
    fn test_dzu() {
        assert_eq!(feed_all("dzu"), "づ");
    }

    // ── Small kana ──────────────────────────────────────────────────────────

    #[test]
    fn test_small_kana_x_prefix() {
        assert_eq!(feed_all("xtu"), "っ");
        assert_eq!(feed_all("xtsu"), "っ");
        assert_eq!(feed_all("xya"), "ゃ");
        assert_eq!(feed_all("xyu"), "ゅ");
        assert_eq!(feed_all("xyo"), "ょ");
        assert_eq!(feed_all("xa"), "ぁ");
        assert_eq!(feed_all("xi"), "ぃ");
        assert_eq!(feed_all("xu"), "ぅ");
        assert_eq!(feed_all("xe"), "ぇ");
        assert_eq!(feed_all("xo"), "ぉ");
        assert_eq!(feed_all("xwa"), "ゎ");
    }

    #[test]
    fn test_small_kana_l_prefix() {
        assert_eq!(feed_all("ltu"), "っ");
        assert_eq!(feed_all("ltsu"), "っ");
        assert_eq!(feed_all("lya"), "ゃ");
        assert_eq!(feed_all("lyu"), "ゅ");
        assert_eq!(feed_all("lyo"), "ょ");
        assert_eq!(feed_all("la"), "ぁ");
        assert_eq!(feed_all("li"), "ぃ");
        assert_eq!(feed_all("lu"), "ぅ");
        assert_eq!(feed_all("le"), "ぇ");
        assert_eq!(feed_all("lo"), "ぉ");
        assert_eq!(feed_all("lwa"), "ゎ");
    }

    #[test]
    fn test_b_row() {
        assert_eq!(feed_all("babibubebo"), "ばびぶべぼ");
    }

    #[test]
    fn test_p_row() {
        assert_eq!(feed_all("papipupepo"), "ぱぴぷぺぽ");
    }

    // ── Combo / digraph tests ─────────────────────────────────────────────────

    #[test]
    fn test_kya_kyu_kyo() {
        assert_eq!(feed_all("kyakyukyo"), "きゃきゅきょ");
    }

    #[test]
    fn test_sha_shu_sho() {
        assert_eq!(feed_all("shashu"), "しゃしゅ");
        assert_eq!(feed_all("sho"), "しょ");
    }

    #[test]
    fn test_cha_chu_cho() {
        assert_eq!(feed_all("chachu"), "ちゃちゅ");
        assert_eq!(feed_all("cho"), "ちょ");
    }

    #[test]
    fn test_nya_nyu_nyo() {
        assert_eq!(feed_all("nyanyu"), "にゃにゅ");
        assert_eq!(feed_all("nyo"), "にょ");
    }

    #[test]
    fn test_ja_ju_jo() {
        assert_eq!(feed_all("jajujo"), "じゃじゅじょ");
    }

    #[test]
    fn test_rya_ryu_ryo() {
        assert_eq!(feed_all("ryaryuryo"), "りゃりゅりょ");
    }

    #[test]
    fn test_gya_gyu_gyo() {
        assert_eq!(feed_all("gyagyugyo"), "ぎゃぎゅぎょ");
    }

    #[test]
    fn test_bya_pya() {
        assert_eq!(feed_all("bya"), "びゃ");
        assert_eq!(feed_all("pya"), "ぴゃ");
    }

    // ── Double consonant (っ insertion) ───────────────────────────────────────

    #[test]
    fn test_double_k() {
        assert_eq!(feed_all("kka"), "っか");
    }

    #[test]
    fn test_double_s() {
        assert_eq!(feed_all("ssa"), "っさ");
    }

    #[test]
    fn test_double_t() {
        assert_eq!(feed_all("tte"), "って");
    }

    #[test]
    fn test_double_p() {
        assert_eq!(feed_all("ppo"), "っぽ");
    }

    #[test]
    fn test_double_in_word() {
        // "kekka" → け + っか
        assert_eq!(feed_all("kekka"), "けっか");
        // "zassou" → ざっそう
        assert_eq!(feed_all("zassou"), "ざっそう");
    }

    // ── n rules ───────────────────────────────────────────────────────────────

    #[test]
    fn test_nn() {
        // "nn" emits exactly one ん.  The second 'n' is kept pending (so that
        // "onna" → おんな works) but is suppressed on flush because it
        // originated from the "nn" sequence.
        assert_eq!(feed_all("nn"), "ん");
    }

    #[test]
    fn test_n_standalone_flush() {
        // A single trailing 'n' flushes as ん.
        assert_eq!(feed_all("n"), "ん");
    }

    #[test]
    fn test_n_before_consonant() {
        // 'n' followed by 'k' → ん + か
        assert_eq!(feed_all("nka"), "んか");
        // 'n' followed by 'p' → ん + ぽ
        assert_eq!(feed_all("npo"), "んぽ");
    }

    #[test]
    fn test_n_not_swallowed_by_vowel() {
        // "na" should be な, NOT ん + あ
        assert_eq!(feed_all("na"), "な");
        assert_eq!(feed_all("ni"), "に");
        assert_eq!(feed_all("nu"), "ぬ");
        assert_eq!(feed_all("ne"), "ね");
        assert_eq!(feed_all("no"), "の");
    }

    #[test]
    fn test_n_end_of_input() {
        // A trailing 'n' should flush as ん
        assert_eq!(feed_all("hon"), "ほん");
        assert_eq!(feed_all("n"), "ん");
    }

    #[test]
    fn test_nin() {
        // "nin" → に + n(pending) → flush → にん
        assert_eq!(feed_all("nin"), "にん");
    }

    #[test]
    fn test_onnna_ambiguous_nn() {
        // "onna" → o(お) + nn(ん) + na(な) = おんな
        assert_eq!(feed_all("onna"), "おんな");
    }

    // ── Special symbols ───────────────────────────────────────────────────────

    #[test]
    fn test_hyphen_to_chouonpu() {
        assert_eq!(feed_all("-"), "ー");
        assert_eq!(feed_all("to-kyou"), "とーきょう");
    }

    #[test]
    fn test_period_to_kuten() {
        assert_eq!(feed_all("."), "。");
    }

    #[test]
    fn test_comma_to_touten() {
        assert_eq!(feed_all(","), "、");
    }

    // ── Backspace handling ────────────────────────────────────────────────────

    #[test]
    fn test_backspace_removes_pending() {
        let mut conv = RomajiConverter::new();
        conv.feed('k');
        assert_eq!(conv.pending(), "k");
        let out = conv.backspace();
        assert_eq!(out.pending, "");
        assert_eq!(out.confirmed, None);
        assert_eq!(conv.pending(), "");
    }

    #[test]
    fn test_backspace_on_empty_is_noop() {
        let mut conv = RomajiConverter::new();
        let out = conv.backspace();
        assert_eq!(out.pending, "");
        assert_eq!(out.confirmed, None);
    }

    #[test]
    fn test_backspace_then_continue() {
        let mut conv = RomajiConverter::new();
        conv.feed('k');
        conv.backspace();
        // After erasing 'k', typing 's' + 'a' should give さ
        conv.feed('s');
        let out = conv.feed('a');
        assert_eq!(out.confirmed.as_deref(), Some("さ"));
        assert_eq!(out.pending, "");
    }

    #[test]
    fn test_backspace_multi_char_pending() {
        let mut conv = RomajiConverter::new();
        conv.feed('s');
        conv.feed('h');
        assert_eq!(conv.pending(), "sh");
        let out = conv.backspace();
        assert_eq!(out.pending, "s");
        // Now complete with 'a' → さ (since "sa" matches, not "sha")
        let out2 = conv.feed('a');
        assert_eq!(out2.confirmed.as_deref(), Some("さ"));
    }

    // ── Reset ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_reset_clears_state() {
        let mut conv = RomajiConverter::new();
        conv.feed('k');
        conv.feed('y');
        assert_eq!(conv.pending(), "ky");
        conv.reset();
        assert_eq!(conv.pending(), "");
        // After reset, normal input should work
        let out = conv.feed('a');
        assert_eq!(out.confirmed.as_deref(), Some("あ"));
    }

    // ── Per-character output verification ────────────────────────────────────

    #[test]
    fn test_feed_incremental_ka() {
        let mut conv = RomajiConverter::new();
        let out1 = conv.feed('k');
        assert_eq!(out1.confirmed, None);
        assert_eq!(out1.pending, "k");

        let out2 = conv.feed('a');
        assert_eq!(out2.confirmed.as_deref(), Some("か"));
        assert_eq!(out2.pending, "");
    }

    #[test]
    fn test_feed_incremental_tsu() {
        let mut conv = RomajiConverter::new();
        let out1 = conv.feed('t');
        assert_eq!(out1.confirmed, None);
        assert_eq!(out1.pending, "t");

        let out2 = conv.feed('s');
        assert_eq!(out2.confirmed, None);
        assert_eq!(out2.pending, "ts");

        let out3 = conv.feed('u');
        assert_eq!(out3.confirmed.as_deref(), Some("つ"));
        assert_eq!(out3.pending, "");
    }

    #[test]
    fn test_feed_double_consonant_incremental() {
        let mut conv = RomajiConverter::new();
        // First 'k' – valid prefix
        let out1 = conv.feed('k');
        assert_eq!(out1.confirmed, None);
        assert_eq!(out1.pending, "k");

        // Second 'k' – triggers っ, new pending is "k"
        let out2 = conv.feed('k');
        assert_eq!(out2.confirmed.as_deref(), Some("っ"));
        assert_eq!(out2.pending, "k");

        // 'a' completes か
        let out3 = conv.feed('a');
        assert_eq!(out3.confirmed.as_deref(), Some("か"));
        assert_eq!(out3.pending, "");
    }

    // ── Miscellaneous real words ───────────────────────────────────────────────

    #[test]
    fn test_nihongo() {
        assert_eq!(feed_all("nihongo"), "にほんご");
    }

    #[test]
    fn test_gakkou() {
        // "gakkou" → が + っ + こ + う
        assert_eq!(feed_all("gakkou"), "がっこう");
    }

    #[test]
    fn test_tabemono() {
        assert_eq!(feed_all("tabemono"), "たべもの");
    }

    #[test]
    fn test_watashi() {
        assert_eq!(feed_all("watashi"), "わたし");
    }

    #[test]
    fn test_ookii() {
        // "ookii" → お + お + き + い
        assert_eq!(feed_all("ookii"), "おおきい");
    }
}
