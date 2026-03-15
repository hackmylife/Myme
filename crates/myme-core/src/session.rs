//! Per-input-session state management.
//!
//! ## Responsibilities
//!
//! - Own the live romaji input buffer and the derived kana preedit string.
//! - Drive the [`crate::romaji`] state machine as keystrokes arrive.
//! - Segment the preedit string into conversion units and coordinate with
//!   [`crate::dictionary`] + [`crate::candidate`] to resolve each segment.
//! - Expose a clean event-driven API:
//!     - `handle_key(key: KeyEvent, dict: &dyn DictionaryLookup) -> SessionAction`
//!     - `reset()`
//! - Be the single source of truth for what the IME client (macOS plug-in via
//!   [`crate::ffi`]) needs to render: preedit text, cursor position, and the
//!   active candidate window contents.
//!
//! ## Non-goals
//!
//! This module does **not** call any macOS or platform API; all platform
//! interaction is mediated through [`crate::ffi`].

use crate::candidate::Candidate;
use crate::dictionary::DictionaryLookup;
use crate::learning::LearningStore;
use crate::romaji::RomajiConverter;
use crate::segmenter::{self, Segment};

// ---------------------------------------------------------------------------
// SegmentInfo
// ---------------------------------------------------------------------------

/// Information about a single segment in the conversion display.
///
/// Used by the UI layer to render per-segment underlines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentInfo {
    /// The surface form currently selected for this segment.
    pub surface: String,
    /// Whether this is the active (focused) segment.
    pub is_active: bool,
}

// ---------------------------------------------------------------------------
// KeyEvent
// ---------------------------------------------------------------------------

/// A normalised keyboard event delivered to the session state machine.
///
/// Platform layers translate their native key representations into this enum
/// before calling [`Session::handle_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyEvent {
    /// A printable character (letters, digits, punctuation handled by romaji).
    Character(char),
    /// Space bar – triggers conversion lookup or cycles candidates.
    Space,
    /// Return/Enter – commits the current selection.
    Enter,
    /// Backspace – removes the last input unit.
    Backspace,
    /// Escape – cancels the current operation.
    Escape,
    /// Up-arrow – moves the candidate selection upward.
    ArrowUp,
    /// Down-arrow – moves the candidate selection downward.
    ArrowDown,
    /// Left-arrow – moves to the previous segment during conversion.
    ArrowLeft,
    /// Right-arrow – moves to the next segment during conversion.
    ArrowRight,
    /// Digit 1–9 – selects a candidate by position (1-based).
    Number(u8),
}

// ---------------------------------------------------------------------------
// SessionState
// ---------------------------------------------------------------------------

/// The current phase of the input session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// No active preedit; the session is waiting for the first keystroke.
    Idle,
    /// The user is typing romaji; a kana preedit is being built incrementally.
    Composing,
    /// A dictionary lookup has been performed and the candidate window is open.
    Converting,
}

// ---------------------------------------------------------------------------
// SessionAction
// ---------------------------------------------------------------------------

/// The action the IME client must perform after a key event is processed.
///
/// The caller should inspect the returned variant and update the UI
/// accordingly.  The session itself does not reach out to any platform API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionAction {
    /// Nothing changed that requires a UI update.
    Noop,
    /// The preedit string has changed; re-render the underlined in-progress text.
    ///
    /// - `text`: the confirmed kana portion accumulated so far.
    /// - `pending_romaji`: the not-yet-resolved romaji suffix (e.g. `"sh"`).
    UpdatePreedit { text: String, pending_romaji: String },
    /// The candidate window should be shown (or its contents refreshed).
    ///
    /// - `segments`: per-segment info (surface + active flag) for preedit display.
    /// - `active_segment`: zero-based index of the focused segment.
    /// - `candidates`: ordered list for the active segment (best first).
    /// - `selected`: index of the currently highlighted candidate within the active segment.
    /// - `preedit`: the full kana reading being converted (for backward compat).
    ShowCandidates {
        segments: Vec<SegmentInfo>,
        active_segment: usize,
        candidates: Vec<Candidate>,
        selected: usize,
        preedit: String,
    },
    /// Insert `text` into the document and return to [`SessionState::Idle`].
    Commit(String),
    /// Discard the preedit and return to [`SessionState::Idle`].
    Cancel,
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A single IME input session.
///
/// One session corresponds to one focused text field.  It owns a
/// [`RomajiConverter`] and all transient state (kana buffer, candidate list,
/// selection index).
///
/// # Usage
///
/// ```rust
/// use myme_core::session::{Session, KeyEvent, SessionAction};
/// use myme_core::dictionary::SimpleDictionary;
///
/// let dict = SimpleDictionary::load_from_skk_text("").unwrap();
/// let mut session = Session::new();
/// let action = session.handle_key(KeyEvent::Character('a'), &dict, None);
/// // action == SessionAction::UpdatePreedit { text: "あ", pending_romaji: "" }
/// ```
pub struct Session {
    /// Current phase of the state machine.
    state: SessionState,
    /// Incremental romaji → kana converter.
    romaji: RomajiConverter,
    /// Kana characters that have been fully resolved from romaji input and are
    /// waiting to be either committed or sent for dictionary lookup.
    confirmed_kana: String,
    /// Conversion segments during Converting state.
    segments: Vec<Segment>,
    /// Zero-based index of the active (focused) segment.
    active_segment: usize,
}

impl Session {
    /// Creates a new, idle session.
    pub fn new() -> Self {
        Self {
            state: SessionState::Idle,
            romaji: RomajiConverter::new(),
            confirmed_kana: String::new(),
            segments: Vec::new(),
            active_segment: 0,
        }
    }

    /// Returns the current state of the session.
    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// Resets the session to [`SessionState::Idle`], discarding all pending
    /// input.
    pub fn reset(&mut self) {
        self.state = SessionState::Idle;
        self.romaji.reset();
        self.confirmed_kana.clear();
        self.segments.clear();
        self.active_segment = 0;
    }

    /// Processes one key event and returns the action the caller should perform.
    ///
    /// The `dict` parameter is borrowed only for the duration of this call so
    /// that the caller retains ownership of the dictionary.
    ///
    /// If `learning` is provided, selection history is automatically recorded
    /// when a conversion is committed.
    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        dict: &dyn DictionaryLookup,
        learning: Option<&mut LearningStore>,
    ) -> SessionAction {
        // Capture segment data before it's cleared by reset (for learning).
        let pre_segments: Vec<(String, String)> = if self.state == SessionState::Converting {
            self.segments
                .iter()
                .map(|seg| (seg.reading.clone(), seg.selected_surface().to_string()))
                .collect()
        } else {
            Vec::new()
        };

        let action = {
            // Reborrow learning immutably for boost application during conversion.
            // The block scope ensures this borrow ends before the mutable use below.
            let learning_ref = learning.as_deref();
            match &self.state {
                SessionState::Idle => self.handle_idle(key, dict),
                SessionState::Composing => self.handle_composing(key, dict, learning_ref),
                SessionState::Converting => self.handle_converting(key, dict),
            }
        };

        // Record learning data when a conversion is committed from Converting state.
        if let SessionAction::Commit(_) = &action {
            if let Some(learning) = learning {
                for (reading, surface) in &pre_segments {
                    learning.record(reading, surface);
                }
            }
        }

        action
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Builds the `UpdatePreedit` action from current converter state.
    fn update_preedit_action(&self) -> SessionAction {
        SessionAction::UpdatePreedit {
            text: self.confirmed_kana.clone(),
            pending_romaji: self.romaji.pending().to_string(),
        }
    }

    /// Builds the `ShowCandidates` action from current session state.
    fn show_candidates_action(&self) -> SessionAction {
        let segment_infos: Vec<SegmentInfo> = self
            .segments
            .iter()
            .enumerate()
            .map(|(i, seg)| SegmentInfo {
                surface: seg.selected_surface().to_string(),
                is_active: i == self.active_segment,
            })
            .collect();

        let active_seg = &self.segments[self.active_segment];

        SessionAction::ShowCandidates {
            segments: segment_infos,
            active_segment: self.active_segment,
            candidates: active_seg.candidates.clone(),
            selected: active_seg.selected,
            preedit: self.confirmed_kana.clone(),
        }
    }

    /// Performs segmentation on `confirmed_kana` and transitions to
    /// `Converting` state.  Returns `ShowCandidates` if any segment has
    /// candidates, otherwise stays in `Composing` and returns `Noop`.
    fn try_convert(
        &mut self,
        dict: &dyn DictionaryLookup,
        learning: Option<&LearningStore>,
    ) -> SessionAction {
        let segments = segmenter::segment(&self.confirmed_kana, dict);
        if segments.is_empty() {
            return SessionAction::Noop;
        }
        self.segments = segments;
        self.active_segment = 0;
        if let Some(learning) = learning {
            self.apply_learning_boosts(learning);
        }
        self.state = SessionState::Converting;
        self.show_candidates_action()
    }

    /// Applies learning boosts to all candidates in all segments, re-sorts,
    /// and resets selection indices.
    fn apply_learning_boosts(&mut self, learning: &LearningStore) {
        for seg in &mut self.segments {
            for cand in &mut seg.candidates {
                let boost = learning.boost(&seg.reading, &cand.surface);
                if boost > 0 {
                    cand.score += boost;
                    cand.source = crate::candidate::CandidateSource::Learning;
                }
            }
            seg.candidates.sort();
            seg.selected = 0;
        }
    }

    // -----------------------------------------------------------------------
    // State handlers
    // -----------------------------------------------------------------------

    /// Handles a key event while in [`SessionState::Idle`].
    fn handle_idle(&mut self, key: KeyEvent, _dict: &dyn DictionaryLookup) -> SessionAction {
        match key {
            KeyEvent::Character(ch) => {
                // Any printable character starts composing.
                self.state = SessionState::Composing;
                let out = self.romaji.feed(ch);
                if let Some(kana) = out.confirmed {
                    self.confirmed_kana.push_str(&kana);
                }
                self.update_preedit_action()
            }
            // All other keys are ignored while idle – nothing to act on.
            _ => SessionAction::Noop,
        }
    }

    /// Handles a key event while in [`SessionState::Composing`].
    fn handle_composing(
        &mut self,
        key: KeyEvent,
        dict: &dyn DictionaryLookup,
        learning: Option<&LearningStore>,
    ) -> SessionAction {
        match key {
            KeyEvent::Character(ch) => {
                let out = self.romaji.feed(ch);
                if let Some(kana) = out.confirmed {
                    self.confirmed_kana.push_str(&kana);
                }
                self.update_preedit_action()
            }

            KeyEvent::Space => {
                // Flush any pending romaji before looking up.
                self.flush_pending_romaji();
                if self.confirmed_kana.is_empty() {
                    // Nothing to convert.
                    return SessionAction::Noop;
                }
                self.try_convert(dict, learning)
            }

            KeyEvent::Enter => {
                // Flush pending romaji then commit the kana as-is.
                self.flush_pending_romaji();
                let text = std::mem::take(&mut self.confirmed_kana);
                if text.is_empty() {
                    self.state = SessionState::Idle;
                    return SessionAction::Noop;
                }
                self.state = SessionState::Idle;
                self.romaji.reset();
                SessionAction::Commit(text)
            }

            KeyEvent::Backspace => {
                if !self.romaji.pending().is_empty() {
                    // There is pending romaji – erase one romaji character.
                    self.romaji.backspace();
                    self.update_preedit_action()
                } else if !self.confirmed_kana.is_empty() {
                    // No pending romaji – remove the last kana scalar.
                    let new_len = self
                        .confirmed_kana
                        .char_indices()
                        .next_back()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.confirmed_kana.truncate(new_len);

                    if self.confirmed_kana.is_empty() {
                        // Nothing left; return to idle.
                        self.state = SessionState::Idle;
                        SessionAction::Cancel
                    } else {
                        self.update_preedit_action()
                    }
                } else {
                    // Both buffers empty – return to idle.
                    self.state = SessionState::Idle;
                    SessionAction::Cancel
                }
            }

            KeyEvent::Escape => {
                self.reset();
                SessionAction::Cancel
            }

            // Keys that have no defined action in Composing state.
            _ => SessionAction::Noop,
        }
    }

    /// Returns the committed text by joining all segments' selected surfaces.
    fn commit_all_segments(&self) -> String {
        self.segments
            .iter()
            .map(|seg| seg.selected_surface())
            .collect()
    }

    /// Handles a key event while in [`SessionState::Converting`].
    fn handle_converting(&mut self, key: KeyEvent, _dict: &dyn DictionaryLookup) -> SessionAction {
        match key {
            KeyEvent::Space | KeyEvent::ArrowDown => {
                // Advance to the next candidate within the active segment (wrapping).
                let seg = &mut self.segments[self.active_segment];
                if !seg.candidates.is_empty() {
                    seg.selected = (seg.selected + 1) % seg.candidates.len();
                }
                self.show_candidates_action()
            }

            KeyEvent::ArrowUp => {
                // Move to the previous candidate within the active segment (wrapping).
                let seg = &mut self.segments[self.active_segment];
                if !seg.candidates.is_empty() {
                    seg.selected = if seg.selected == 0 {
                        seg.candidates.len() - 1
                    } else {
                        seg.selected - 1
                    };
                }
                self.show_candidates_action()
            }

            KeyEvent::ArrowRight => {
                // Move to the next segment.
                if !self.segments.is_empty() {
                    self.active_segment = (self.active_segment + 1) % self.segments.len();
                }
                self.show_candidates_action()
            }

            KeyEvent::ArrowLeft => {
                // Move to the previous segment.
                if !self.segments.is_empty() {
                    self.active_segment = if self.active_segment == 0 {
                        self.segments.len() - 1
                    } else {
                        self.active_segment - 1
                    };
                }
                self.show_candidates_action()
            }

            KeyEvent::Enter => {
                // Commit all segments joined.
                let text = self.commit_all_segments();
                self.reset();
                SessionAction::Commit(text)
            }

            KeyEvent::Number(n) if (1..=9).contains(&n) => {
                let idx = (n - 1) as usize;
                let seg = &self.segments[self.active_segment];
                if idx < seg.candidates.len() {
                    // Select the candidate by number for the active segment,
                    // then commit all segments.
                    self.segments[self.active_segment].selected = idx;
                    let text = self.commit_all_segments();
                    self.reset();
                    SessionAction::Commit(text)
                } else {
                    // Number out of range – ignore.
                    self.show_candidates_action()
                }
            }

            KeyEvent::Escape | KeyEvent::Backspace => {
                // Cancel conversion; return to composing with the kana buffer
                // intact so the user can keep editing.
                self.segments.clear();
                self.active_segment = 0;
                self.state = SessionState::Composing;
                self.update_preedit_action()
            }

            KeyEvent::Character(ch) => {
                // Commit all segments then begin a new composing sequence.
                let text = self.commit_all_segments();

                // Reset internal state, then switch to Composing.
                self.segments.clear();
                self.active_segment = 0;
                self.confirmed_kana.clear();
                self.romaji.reset();
                self.state = SessionState::Composing;

                // Feed the new character into the fresh composer.
                let out = self.romaji.feed(ch);
                if let Some(kana) = out.confirmed {
                    self.confirmed_kana.push_str(&kana);
                }

                SessionAction::Commit(text)
            }

            // Unhandled keys.
            _ => self.show_candidates_action(),
        }
    }

    /// Flushes any pending romaji into `confirmed_kana`.
    ///
    /// A lone trailing `"n"` is resolved as `"ん"`.  Any other non-empty
    /// pending content is appended verbatim (this covers unusual but legal
    /// sequences that never resolved to kana).
    fn flush_pending_romaji(&mut self) {
        let pending = self.romaji.pending().to_string();
        if pending.is_empty() {
            return;
        }
        if pending == "n" && !self.romaji.is_nn_pending() {
            // Lone trailing "n" that is NOT left over from "nn" → emit ん.
            self.confirmed_kana.push('ん');
        } else if pending == "n" && self.romaji.is_nn_pending() {
            // Pending "n" left from "nn" → suppress (the ん was already emitted).
        } else {
            self.confirmed_kana.push_str(&pending);
        }
        self.romaji.reset();
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{Candidate, CandidateSource};

    // -----------------------------------------------------------------------
    // Mock dictionary
    // -----------------------------------------------------------------------

    /// A simple mock dictionary that returns hard-coded candidates for a small
    /// set of known readings and an empty list for everything else.
    struct MockDictionary;

    impl DictionaryLookup for MockDictionary {
        fn lookup(&self, reading: &str) -> Vec<Candidate> {
            match reading {
                "へんかん" => vec![
                    Candidate::new("変換", "へんかん", 30, CandidateSource::System),
                    Candidate::new("偏官", "へんかん", 20, CandidateSource::System),
                    Candidate::new("返還", "へんかん", 10, CandidateSource::System),
                ],
                "かな" => vec![
                    Candidate::new("仮名", "かな", 20, CandidateSource::System),
                    Candidate::new("かな", "かな", 10, CandidateSource::System),
                ],
                "てすと" => vec![
                    Candidate::new("テスト", "てすと", 30, CandidateSource::System),
                    Candidate::new("試験", "てすと", 20, CandidateSource::System),
                ],
                "あい" => vec![
                    Candidate::new("愛", "あい", 40, CandidateSource::System),
                    Candidate::new("相", "あい", 30, CandidateSource::System),
                    Candidate::new("藍", "あい", 20, CandidateSource::System),
                    Candidate::new("哀", "あい", 10, CandidateSource::System),
                ],
                "きょう" => vec![
                    Candidate::new("今日", "きょう", 30, CandidateSource::System),
                    Candidate::new("京", "きょう", 20, CandidateSource::System),
                ],
                "は" => vec![
                    Candidate::new("は", "は", 10, CandidateSource::System),
                ],
                _ => vec![],
            }
        }
    }

    /// Feeds a sequence of `Character` key events for each char in `input`.
    fn feed_chars(session: &mut Session, dict: &dyn DictionaryLookup, input: &str) -> SessionAction {
        let mut last = SessionAction::Noop;
        for ch in input.chars() {
            last = session.handle_key(KeyEvent::Character(ch), dict, None);
        }
        last
    }

    // -----------------------------------------------------------------------
    // 1. Full conversion cycle: type → Space → navigate → Enter
    // -----------------------------------------------------------------------

    #[test]
    fn test_full_conversion_cycle_henkan() {
        let dict = MockDictionary;
        let mut session = Session::new();

        // Type "henkan"
        feed_chars(&mut session, &dict, "henkan");
        assert_eq!(session.state(), &SessionState::Composing);

        // Press Space → should show candidates for へんかん
        let action = session.handle_key(KeyEvent::Space, &dict, None);
        assert_eq!(session.state(), &SessionState::Converting);
        match action {
            SessionAction::ShowCandidates { candidates, selected, preedit, .. } => {
                assert_eq!(selected, 0);
                assert_eq!(preedit, "へんかん");
                assert_eq!(candidates[0].surface, "変換");
                assert_eq!(candidates.len(), 5); // 3 dict + katakana + hiragana fallbacks
            }
            _ => panic!("expected ShowCandidates, got {:?}", action),
        }

        // Press Enter → commit the first (selected) candidate
        let commit = session.handle_key(KeyEvent::Enter, &dict, None);
        assert_eq!(session.state(), &SessionState::Idle);
        assert_eq!(commit, SessionAction::Commit("変換".to_string()));
    }

    // -----------------------------------------------------------------------
    // 2. Kana commit: type "kana" then Enter (commit hiragana directly)
    // -----------------------------------------------------------------------

    #[test]
    fn test_kana_commit_with_enter() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "kana");
        let action = session.handle_key(KeyEvent::Enter, &dict, None);

        assert_eq!(session.state(), &SessionState::Idle);
        assert_eq!(action, SessionAction::Commit("かな".to_string()));
    }

    // -----------------------------------------------------------------------
    // 3. Cancel with Escape during composing
    // -----------------------------------------------------------------------

    #[test]
    fn test_escape_during_composing_cancels() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "te");
        let action = session.handle_key(KeyEvent::Escape, &dict, None);

        assert_eq!(session.state(), &SessionState::Idle);
        assert_eq!(action, SessionAction::Cancel);
    }

    // -----------------------------------------------------------------------
    // 4. Backspace removes pending romaji one character at a time
    // -----------------------------------------------------------------------

    #[test]
    fn test_backspace_removes_pending_romaji() {
        let dict = MockDictionary;
        let mut session = Session::new();

        // 'k' is pending romaji
        session.handle_key(KeyEvent::Character('k'), &dict, None);
        assert_eq!(session.state(), &SessionState::Composing);

        let action = session.handle_key(KeyEvent::Backspace, &dict, None);
        // Pending romaji cleared; confirmed_kana is empty → still Composing
        // (the preedit update is returned)
        match action {
            SessionAction::UpdatePreedit { text, pending_romaji } => {
                assert_eq!(text, "");
                assert_eq!(pending_romaji, "");
            }
            _ => panic!("expected UpdatePreedit, got {:?}", action),
        }
    }

    // -----------------------------------------------------------------------
    // 5. Backspace removes the last kana character when no pending romaji
    // -----------------------------------------------------------------------

    #[test]
    fn test_backspace_removes_last_kana() {
        let dict = MockDictionary;
        let mut session = Session::new();

        // Type "ka" → "か" is confirmed, nothing pending
        feed_chars(&mut session, &dict, "kana");
        // confirmed_kana is "かな", pending is ""

        let action = session.handle_key(KeyEvent::Backspace, &dict, None);
        match action {
            SessionAction::UpdatePreedit { text, pending_romaji } => {
                assert_eq!(text, "か");
                assert_eq!(pending_romaji, "");
            }
            _ => panic!("expected UpdatePreedit, got {:?}", action),
        }
    }

    // -----------------------------------------------------------------------
    // 6. Backspace on empty preedit returns to Idle with Cancel
    // -----------------------------------------------------------------------

    #[test]
    fn test_backspace_on_single_kana_returns_idle() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "ka");
        // confirmed_kana = "か", pending = ""

        let action = session.handle_key(KeyEvent::Backspace, &dict, None);
        // Removes "か", leaving empty → should return to Idle
        assert_eq!(session.state(), &SessionState::Idle);
        assert_eq!(action, SessionAction::Cancel);
    }

    // -----------------------------------------------------------------------
    // 7. Candidate navigation with Space cycles forward
    // -----------------------------------------------------------------------

    #[test]
    fn test_space_cycles_candidates_forward() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "henkan");
        session.handle_key(KeyEvent::Space, &dict, None); // now Converting, selected=0

        // Second Space → move to next candidate
        let action = session.handle_key(KeyEvent::Space, &dict, None);
        match action {
            SessionAction::ShowCandidates { selected, .. } => {
                assert_eq!(selected, 1);
            }
            _ => panic!("expected ShowCandidates, got {:?}", action),
        }
    }

    // -----------------------------------------------------------------------
    // 8. ArrowDown and ArrowUp navigate candidates
    // -----------------------------------------------------------------------

    #[test]
    fn test_arrow_keys_navigate_candidates() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "henkan");
        session.handle_key(KeyEvent::Space, &dict, None); // selected=0

        let down1 = session.handle_key(KeyEvent::ArrowDown, &dict, None);
        match &down1 {
            SessionAction::ShowCandidates { selected, .. } => assert_eq!(*selected, 1),
            _ => panic!("expected ShowCandidates"),
        }

        let up1 = session.handle_key(KeyEvent::ArrowUp, &dict, None);
        match &up1 {
            SessionAction::ShowCandidates { selected, .. } => assert_eq!(*selected, 0),
            _ => panic!("expected ShowCandidates"),
        }
    }

    // -----------------------------------------------------------------------
    // 9. ArrowUp wraps from first to last candidate
    // -----------------------------------------------------------------------

    #[test]
    fn test_arrow_up_wraps_to_last() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "henkan");
        session.handle_key(KeyEvent::Space, &dict, None); // selected=0

        let action = session.handle_key(KeyEvent::ArrowUp, &dict, None);
        match action {
            SessionAction::ShowCandidates { selected, candidates, .. } => {
                // Should wrap to the last candidate (index 2 for 3-candidate list)
                assert_eq!(selected, candidates.len() - 1);
            }
            _ => panic!("expected ShowCandidates"),
        }
    }

    // -----------------------------------------------------------------------
    // 10. Number key commits the nth candidate (1-based)
    // -----------------------------------------------------------------------

    #[test]
    fn test_number_key_selects_candidate() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "henkan");
        session.handle_key(KeyEvent::Space, &dict, None);

        // Press '2' → commit candidate at index 1 ("偏官")
        let action = session.handle_key(KeyEvent::Number(2), &dict, None);
        assert_eq!(session.state(), &SessionState::Idle);
        assert_eq!(action, SessionAction::Commit("偏官".to_string()));
    }

    // -----------------------------------------------------------------------
    // 11. Number key out of range is ignored (stays in Converting)
    // -----------------------------------------------------------------------

    #[test]
    fn test_number_key_out_of_range_is_ignored() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "henkan");
        session.handle_key(KeyEvent::Space, &dict, None);

        // 9 is out of range for a 3-candidate list
        let action = session.handle_key(KeyEvent::Number(9), &dict, None);
        assert_eq!(session.state(), &SessionState::Converting);
        match action {
            SessionAction::ShowCandidates { .. } => {}
            _ => panic!("expected ShowCandidates"),
        }
    }

    // -----------------------------------------------------------------------
    // 12. Escape in Converting returns to Composing with kana intact
    // -----------------------------------------------------------------------

    #[test]
    fn test_escape_in_converting_returns_to_composing() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "henkan");
        session.handle_key(KeyEvent::Space, &dict, None); // Converting

        let action = session.handle_key(KeyEvent::Escape, &dict, None);
        assert_eq!(session.state(), &SessionState::Composing);
        match action {
            SessionAction::UpdatePreedit { text, pending_romaji } => {
                assert_eq!(text, "へんかん");
                assert_eq!(pending_romaji, "");
            }
            _ => panic!("expected UpdatePreedit, got {:?}", action),
        }
    }

    // -----------------------------------------------------------------------
    // 13. Backspace in Converting behaves like Escape (return to Composing)
    // -----------------------------------------------------------------------

    #[test]
    fn test_backspace_in_converting_returns_to_composing() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "kana");
        session.handle_key(KeyEvent::Space, &dict, None); // Converting

        let action = session.handle_key(KeyEvent::Backspace, &dict, None);
        assert_eq!(session.state(), &SessionState::Composing);
        match action {
            SessionAction::UpdatePreedit { text, .. } => {
                assert_eq!(text, "かな");
            }
            _ => panic!("expected UpdatePreedit, got {:?}", action),
        }
    }

    // -----------------------------------------------------------------------
    // 14. Space when no dictionary match enters Converting with self-candidate
    // -----------------------------------------------------------------------

    #[test]
    fn test_space_with_no_dict_match_converts_with_self_candidate() {
        let dict = MockDictionary;
        let mut session = Session::new();

        // "す" is not in mock dict, but the segmenter will produce a single
        // segment with the character itself as the sole candidate.
        feed_chars(&mut session, &dict, "su"); // "す"
        let action = session.handle_key(KeyEvent::Space, &dict, None);

        assert_eq!(session.state(), &SessionState::Converting);
        match action {
            SessionAction::ShowCandidates { candidates, .. } => {
                assert_eq!(candidates.len(), 2); // self-candidate + katakana fallback
                assert_eq!(candidates[0].surface, "す");
            }
            _ => panic!("expected ShowCandidates, got {:?}", action),
        }
    }

    // -----------------------------------------------------------------------
    // 15. Empty input edge case: keys in Idle that are not Character are Noop
    // -----------------------------------------------------------------------

    #[test]
    fn test_idle_non_character_keys_are_noop() {
        let dict = MockDictionary;
        let mut session = Session::new();

        assert_eq!(session.handle_key(KeyEvent::Space, &dict, None), SessionAction::Noop);
        assert_eq!(session.handle_key(KeyEvent::Enter, &dict, None), SessionAction::Noop);
        assert_eq!(session.handle_key(KeyEvent::Backspace, &dict, None), SessionAction::Noop);
        assert_eq!(session.handle_key(KeyEvent::Escape, &dict, None), SessionAction::Noop);
        assert_eq!(session.handle_key(KeyEvent::ArrowUp, &dict, None), SessionAction::Noop);
        assert_eq!(session.handle_key(KeyEvent::ArrowDown, &dict, None), SessionAction::Noop);
        assert_eq!(session.state(), &SessionState::Idle);
    }

    // -----------------------------------------------------------------------
    // 16. Converting + Character commits current candidate and starts new compose
    // -----------------------------------------------------------------------

    #[test]
    fn test_character_in_converting_commits_and_restarts() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "henkan");
        session.handle_key(KeyEvent::Space, &dict, None); // selected=0 ("変換")

        // Type a new character while converting
        let action = session.handle_key(KeyEvent::Character('a'), &dict, None);
        // Should commit "変換"
        assert_eq!(action, SessionAction::Commit("変換".to_string()));
        // State should now be Composing (new char 'a' was fed)
        assert_eq!(session.state(), &SessionState::Composing);
    }

    // -----------------------------------------------------------------------
    // 17. Candidate Space cycling wraps around at the end
    // -----------------------------------------------------------------------

    #[test]
    fn test_space_cycles_wrap_around() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "henkan"); // 5 candidates (3 dict + katakana + hiragana)
        session.handle_key(KeyEvent::Space, &dict, None); // selected=0
        session.handle_key(KeyEvent::Space, &dict, None); // selected=1
        session.handle_key(KeyEvent::Space, &dict, None); // selected=2
        session.handle_key(KeyEvent::Space, &dict, None); // selected=3
        session.handle_key(KeyEvent::Space, &dict, None); // selected=4

        // One more Space should wrap back to 0
        let action = session.handle_key(KeyEvent::Space, &dict, None);
        match action {
            SessionAction::ShowCandidates { selected, .. } => assert_eq!(selected, 0),
            _ => panic!("expected ShowCandidates"),
        }
    }

    // -----------------------------------------------------------------------
    // 18. Composing with pending 'n' before Space flushes as ん
    // -----------------------------------------------------------------------

    #[test]
    fn test_pending_n_flushed_on_space() {
        let dict = MockDictionary;
        let mut session = Session::new();

        // Type 'a', 'i' → "あい", then look up
        feed_chars(&mut session, &dict, "ai");

        let action = session.handle_key(KeyEvent::Space, &dict, None);
        assert_eq!(session.state(), &SessionState::Converting);
        match action {
            SessionAction::ShowCandidates { preedit, candidates, .. } => {
                assert_eq!(preedit, "あい");
                assert!(!candidates.is_empty());
                assert_eq!(candidates[0].surface, "愛");
            }
            _ => panic!("expected ShowCandidates, got {:?}", action),
        }
    }

    // -----------------------------------------------------------------------
    // 19. Full multi-step composing and backspace sequence
    // -----------------------------------------------------------------------

    #[test]
    fn test_composing_backspace_sequence() {
        let dict = MockDictionary;
        let mut session = Session::new();

        // Type "sh" (pending), then backspace → pending becomes "s"
        session.handle_key(KeyEvent::Character('s'), &dict, None);
        session.handle_key(KeyEvent::Character('h'), &dict, None);
        let after_bs = session.handle_key(KeyEvent::Backspace, &dict, None);
        match after_bs {
            SessionAction::UpdatePreedit { text, pending_romaji } => {
                assert_eq!(text, "");
                assert_eq!(pending_romaji, "s");
            }
            _ => panic!("expected UpdatePreedit"),
        }

        // Continue typing to confirm "さ"
        session.handle_key(KeyEvent::Character('a'), &dict, None);
        let action = session.handle_key(KeyEvent::Enter, &dict, None);
        assert_eq!(action, SessionAction::Commit("さ".to_string()));
    }

    // -----------------------------------------------------------------------
    // 20. reset() returns to Idle from any state
    // -----------------------------------------------------------------------

    #[test]
    fn test_reset_from_converting() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "henkan");
        session.handle_key(KeyEvent::Space, &dict, None);
        assert_eq!(session.state(), &SessionState::Converting);

        session.reset();
        assert_eq!(session.state(), &SessionState::Idle);

        // After reset, new input should work normally.
        let action = session.handle_key(KeyEvent::Character('a'), &dict, None);
        match action {
            SessionAction::UpdatePreedit { text, pending_romaji } => {
                assert_eq!(text, "あ");
                assert_eq!(pending_romaji, "");
            }
            _ => panic!("expected UpdatePreedit"),
        }
    }

    // -----------------------------------------------------------------------
    // 21. Multi-segment conversion: きょうは → two segments
    // -----------------------------------------------------------------------

    #[test]
    fn test_multi_segment_conversion() {
        let dict = MockDictionary;
        let mut session = Session::new();

        // Type "kyouha" → "きょうは"
        feed_chars(&mut session, &dict, "kyouha");
        let action = session.handle_key(KeyEvent::Space, &dict, None);

        assert_eq!(session.state(), &SessionState::Converting);
        match action {
            SessionAction::ShowCandidates { segments, active_segment, candidates, selected, .. } => {
                assert_eq!(segments.len(), 2);
                assert_eq!(segments[0].surface, "今日"); // first candidate for きょう
                assert_eq!(segments[0].is_active, true);
                assert_eq!(segments[1].surface, "は");
                assert_eq!(segments[1].is_active, false);
                assert_eq!(active_segment, 0);
                assert_eq!(candidates[0].surface, "今日"); // active segment's candidates
                assert_eq!(selected, 0);
            }
            _ => panic!("expected ShowCandidates, got {:?}", action),
        }
    }

    // -----------------------------------------------------------------------
    // 22. Segment navigation with ArrowRight/ArrowLeft
    // -----------------------------------------------------------------------

    #[test]
    fn test_segment_navigation() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "kyouha");
        session.handle_key(KeyEvent::Space, &dict, None); // segments: きょう | は

        // Move to next segment
        let action = session.handle_key(KeyEvent::ArrowRight, &dict, None);
        match action {
            SessionAction::ShowCandidates { active_segment, segments, .. } => {
                assert_eq!(active_segment, 1);
                assert_eq!(segments[0].is_active, false);
                assert_eq!(segments[1].is_active, true);
            }
            _ => panic!("expected ShowCandidates"),
        }

        // Move back to first segment
        let action = session.handle_key(KeyEvent::ArrowLeft, &dict, None);
        match action {
            SessionAction::ShowCandidates { active_segment, .. } => {
                assert_eq!(active_segment, 0);
            }
            _ => panic!("expected ShowCandidates"),
        }
    }

    // -----------------------------------------------------------------------
    // 23. Commit joins all segments
    // -----------------------------------------------------------------------

    #[test]
    fn test_commit_joins_all_segments() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "kyouha");
        session.handle_key(KeyEvent::Space, &dict, None); // segments: 今日 | は

        let action = session.handle_key(KeyEvent::Enter, &dict, None);
        assert_eq!(action, SessionAction::Commit("今日は".to_string()));
        assert_eq!(session.state(), &SessionState::Idle);
    }

    // -----------------------------------------------------------------------
    // 24. Single-segment backward compatibility
    // -----------------------------------------------------------------------

    #[test]
    fn test_single_segment_backward_compat() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "henkan");
        let action = session.handle_key(KeyEvent::Space, &dict, None);

        match action {
            SessionAction::ShowCandidates { segments, candidates, selected, .. } => {
                assert_eq!(segments.len(), 1);
                assert_eq!(segments[0].surface, "変換");
                assert_eq!(candidates.len(), 5); // 3 dict + katakana + hiragana fallbacks
                assert_eq!(selected, 0);
            }
            _ => panic!("expected ShowCandidates"),
        }

        // Enter commits just the one segment
        let action = session.handle_key(KeyEvent::Enter, &dict, None);
        assert_eq!(action, SessionAction::Commit("変換".to_string()));
    }

    // -----------------------------------------------------------------------
    // 25. Candidate cycling within active segment
    // -----------------------------------------------------------------------

    #[test]
    fn test_candidate_cycling_in_segment() {
        let dict = MockDictionary;
        let mut session = Session::new();

        feed_chars(&mut session, &dict, "kyouha");
        session.handle_key(KeyEvent::Space, &dict, None); // active=0 (きょう), selected=0 (今日)

        // Cycle candidates in first segment
        let action = session.handle_key(KeyEvent::Space, &dict, None);
        match action {
            SessionAction::ShowCandidates { segments, selected, .. } => {
                assert_eq!(selected, 1); // 京
                assert_eq!(segments[0].surface, "京");
            }
            _ => panic!("expected ShowCandidates"),
        }

        // Commit → should join with new selection
        let action = session.handle_key(KeyEvent::Enter, &dict, None);
        assert_eq!(action, SessionAction::Commit("京は".to_string()));
    }

    // -----------------------------------------------------------------------
    // 26. Learning records are captured on commit
    // -----------------------------------------------------------------------

    #[test]
    fn test_learning_records_on_commit() {
        let dict = MockDictionary;
        let mut session = Session::new();
        let mut learning = LearningStore::new();

        feed_chars(&mut session, &dict, "kyouha");
        session.handle_key(KeyEvent::Space, &dict, None);

        // Commit with learning
        let action = session.handle_key(KeyEvent::Enter, &dict, Some(&mut learning));
        assert_eq!(action, SessionAction::Commit("今日は".to_string()));

        // Learning should have recorded both segments.
        assert!(learning.boost("きょう", "今日") > 0);
        assert!(learning.boost("は", "は") > 0);
    }

    // -----------------------------------------------------------------------
    // 27. Learning boost promotes a non-default candidate to first
    // -----------------------------------------------------------------------

    #[test]
    fn test_learning_boost_promotes_candidate() {
        let dict = MockDictionary;
        let mut learning = LearningStore::new();

        // Record "京" 5 times so its boost = 50, exceeding "今日"'s base score
        // of 30.  京 has base score 20, so boosted score = 20 + 50 = 70.
        for _ in 0..5 {
            learning.record("きょう", "京");
        }

        let mut session = Session::new();
        feed_chars(&mut session, &dict, "kyou");
        let action = session.handle_key(KeyEvent::Space, &dict, Some(&mut learning));

        match action {
            SessionAction::ShowCandidates { candidates, selected, .. } => {
                // "京" should now be first thanks to the learning boost.
                assert_eq!(selected, 0);
                assert_eq!(candidates[0].surface, "京");
            }
            _ => panic!("expected ShowCandidates, got {:?}", action),
        }
    }

    // -----------------------------------------------------------------------
    // 28. Boosted candidates have CandidateSource::Learning
    // -----------------------------------------------------------------------

    #[test]
    fn test_learning_boost_sets_source() {
        use crate::candidate::CandidateSource;

        let dict = MockDictionary;
        let mut learning = LearningStore::new();

        learning.record("きょう", "京");

        let mut session = Session::new();
        feed_chars(&mut session, &dict, "kyou");
        let action = session.handle_key(KeyEvent::Space, &dict, Some(&mut learning));

        match action {
            SessionAction::ShowCandidates { candidates, .. } => {
                // Find the "京" candidate and verify its source.
                let boosted = candidates.iter().find(|c| c.surface == "京").unwrap();
                assert_eq!(boosted.source, CandidateSource::Learning);
            }
            _ => panic!("expected ShowCandidates"),
        }
    }
}
