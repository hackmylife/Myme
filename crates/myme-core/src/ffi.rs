//! C-ABI foreign-function interface for the macOS InputMethodKit plug-in.
//!
//! ## Responsibilities
//!
//! - Export `extern "C"` functions that the Swift/Objective-C IME extension
//!   calls directly via a `@_silgen_name` or `dlsym` binding.
//! - Convert between C-compatible types (`*const c_char`, `c_int`, raw
//!   pointers to opaque session handles) and the safe Rust types defined in
//!   [`crate::session`].
//! - Provide lifecycle functions:
//!     - `myme_context_new(dict_path) -> *mut MymeContext`
//!     - `myme_context_destroy(ctx: *mut MymeContext)`
//!     - `myme_handle_key(...) -> *mut MymeResult`
//!     - `myme_result_free(result: *mut MymeResult)`
//!     - `myme_get_state(ctx: *const MymeContext) -> MymeState`
//! - Keep the FFI layer thin: no business logic here, only type translation
//!   and `unsafe` boundary management.
//!
//! ## Safety contract
//!
//! All raw pointers received from callers must be non-null and must have been
//! obtained from the corresponding `myme_*` constructor functions.  The caller
//! is responsible for not aliasing context handles across threads without
//! external synchronisation.  Every `*mut MymeResult` returned by
//! `myme_handle_key` **must** be freed with exactly one call to
//! `myme_result_free`; passing it to `free()` or any other deallocator is
//! undefined behaviour.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;

use crate::dictionary::{DictionaryLookup, SimpleDictionary};
use crate::learning::LearningStore;
use crate::session::{KeyEvent, Session, SessionAction, SessionState};
use crate::user_dict::{CompositeDictionary, UserDictionary};

// ---------------------------------------------------------------------------
// Opaque context
// ---------------------------------------------------------------------------

/// Opaque context holding a [`Session`] and its backing [`SimpleDictionary`].
///
/// The Swift caller holds a `*mut MymeContext` obtained from
/// [`myme_context_new`] and passes it back on every subsequent call.  It must
/// never inspect or modify the contents of this struct; the layout is not
/// stable across releases.
pub struct MymeContext {
    session: Session,
    dictionary: Box<dyn DictionaryLookup>,
    learning: Option<LearningStore>,
}

// ---------------------------------------------------------------------------
// C-compatible enums
// ---------------------------------------------------------------------------

/// Key-event discriminant passed from the platform layer to [`myme_handle_key`].
///
/// Mirrors [`KeyEvent`] exactly so that the caller does not need to know about
/// Rust's tagged-union representation.
#[repr(C)]
pub enum MymeKeyType {
    /// A printable character; the `character` parameter carries its Unicode
    /// codepoint.
    Character = 0,
    /// Space bar.
    Space = 1,
    /// Return / Enter.
    Enter = 2,
    /// Backspace / Delete-backward.
    Backspace = 3,
    /// Escape.
    Escape = 4,
    /// Up-arrow.
    ArrowUp = 5,
    /// Down-arrow.
    ArrowDown = 6,
    /// Digit 1–9; the `character` parameter carries the digit value (1–9).
    Number = 7,
    /// Left-arrow.
    ArrowLeft = 8,
    /// Right-arrow.
    ArrowRight = 9,
}

/// Discriminant indicating what the IME client must do after a key event.
///
/// Mirrors [`SessionAction`] exactly.
#[repr(C)]
pub enum MymeActionType {
    /// Nothing changed that requires a UI update.
    Noop = 0,
    /// The preedit string has changed; re-render the underlined in-progress text.
    UpdatePreedit = 1,
    /// The candidate window should be shown or refreshed.
    ShowCandidates = 2,
    /// Insert [`MymeResult::text`] into the document and close the preedit.
    Commit = 3,
    /// Discard the preedit and close any open candidate window.
    Cancel = 4,
}

/// Current phase of the input session; returned by [`myme_get_state`].
///
/// Mirrors [`SessionState`] exactly.
#[repr(C)]
pub enum MymeState {
    /// No active preedit.
    Idle = 0,
    /// User is typing romaji; preedit is being built incrementally.
    Composing = 1,
    /// Dictionary lookup has been performed; candidate window is open.
    Converting = 2,
}

// ---------------------------------------------------------------------------
// Result struct
// ---------------------------------------------------------------------------

/// Heap-allocated result returned by [`myme_handle_key`].
///
/// **Ownership**: the caller receives sole ownership and **must** free this
/// struct with exactly one call to [`myme_result_free`].  All pointer fields
/// (`text`, `pending_romaji`, `candidates`) are owned by this struct and freed
/// together with it.
#[repr(C)]
pub struct MymeResult {
    /// What the IME client must do.
    pub action_type: MymeActionType,
    /// Null-terminated UTF-8 string:
    /// - for `Commit`: the text to insert into the document.
    /// - for `UpdatePreedit`: the confirmed kana portion of the preedit.
    /// - for `ShowCandidates`: the kana string being converted.
    /// - for `Noop` / `Cancel`: empty string (`"\0"`).
    ///
    /// Never null.
    pub text: *const c_char,
    /// Null-terminated UTF-8 string containing the not-yet-resolved romaji
    /// suffix (e.g. `"sh"`).  Empty string when not in `UpdatePreedit`.
    ///
    /// Never null.
    pub pending_romaji: *const c_char,
    /// Pointer to an array of `candidate_count` null-terminated UTF-8 strings,
    /// each representing one conversion candidate.  Null when
    /// `candidate_count` is 0.
    pub candidates: *const *const c_char,
    /// Number of elements in the `candidates` array.
    pub candidate_count: u32,
    /// Zero-based index of the currently selected candidate.  Meaningful only
    /// when `action_type` is `ShowCandidates`.
    pub selected_index: u32,
    /// Pointer to an array of `segment_count` null-terminated UTF-8 strings,
    /// each the selected surface form of one conversion segment.
    /// Null when `segment_count` is 0.  Only set for `ShowCandidates`.
    pub segment_surfaces: *const *const c_char,
    /// Number of segments.  0 when not in `ShowCandidates` (backward compat).
    pub segment_count: u32,
    /// Zero-based index of the currently active (focused) segment.
    pub active_segment: u32,
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Converts a Rust `String` into a heap-allocated `CString` and returns its
/// raw pointer.  The pointer remains valid until [`free_cstring`] is called on
/// it.
///
/// # Panics
///
/// Panics if `s` contains an interior null byte, which is impossible for any
/// string produced by the session layer (all strings are either romaji ASCII or
/// Japanese Unicode that has no null bytes).
/// Returns the path to the learning TSV file, if the home directory is available.
fn dirs_learning_path() -> Option<std::path::PathBuf> {
    std::env::var("HOME").ok().map(|home| {
        std::path::PathBuf::from(home)
            .join("Library/Application Support/myme/learning.tsv")
    })
}

fn into_raw_cstring(s: String) -> *mut c_char {
    // SAFETY: we only call this with strings that do not contain interior nulls.
    // romaji is ASCII, kana is multi-byte UTF-8 with no 0x00 bytes.
    CString::new(s)
        .expect("session strings must not contain interior null bytes")
        .into_raw()
}

/// Frees a `*mut c_char` previously produced by [`into_raw_cstring`].
///
/// # Safety
///
/// `ptr` must be non-null and must have been produced by `CString::into_raw`.
unsafe fn free_cstring(ptr: *mut c_char) {
    // SAFETY: caller guarantees ptr came from CString::into_raw.
    drop(unsafe { CString::from_raw(ptr) });
}

/// Converts a [`SessionAction`] into a heap-allocated [`MymeResult`] and
/// returns its raw pointer.  The caller owns the allocation and must free it
/// with [`myme_result_free`].
fn empty_result(action_type: MymeActionType) -> MymeResult {
    MymeResult {
        action_type,
        text: into_raw_cstring(String::new()),
        pending_romaji: into_raw_cstring(String::new()),
        candidates: std::ptr::null(),
        candidate_count: 0,
        selected_index: 0,
        segment_surfaces: std::ptr::null(),
        segment_count: 0,
        active_segment: 0,
    }
}

fn action_to_result(action: SessionAction) -> *mut MymeResult {
    match action {
        SessionAction::Noop => Box::into_raw(Box::new(empty_result(MymeActionType::Noop))),

        SessionAction::UpdatePreedit { text, pending_romaji } => {
            let mut r = empty_result(MymeActionType::UpdatePreedit);
            r.text = into_raw_cstring(text);
            r.pending_romaji = into_raw_cstring(pending_romaji);
            Box::into_raw(Box::new(r))
        }

        SessionAction::ShowCandidates { segments, active_segment, candidates, selected, preedit } => {
            // Build candidates array.
            let count = candidates.len() as u32;
            let raw_ptrs: Vec<*const c_char> = candidates
                .into_iter()
                .map(|c| into_raw_cstring(c.surface) as *const c_char)
                .collect();
            let boxed_slice = raw_ptrs.into_boxed_slice();
            let array_ptr = Box::into_raw(boxed_slice) as *const *const c_char;

            // Build segment surfaces array.
            let seg_count = segments.len() as u32;
            let seg_ptrs: Vec<*const c_char> = segments
                .into_iter()
                .map(|s| into_raw_cstring(s.surface) as *const c_char)
                .collect();
            let seg_boxed = seg_ptrs.into_boxed_slice();
            let seg_ptr = Box::into_raw(seg_boxed) as *const *const c_char;

            Box::into_raw(Box::new(MymeResult {
                action_type: MymeActionType::ShowCandidates,
                text: into_raw_cstring(preedit),
                pending_romaji: into_raw_cstring(String::new()),
                candidates: array_ptr,
                candidate_count: count,
                selected_index: selected as u32,
                segment_surfaces: seg_ptr,
                segment_count: seg_count,
                active_segment: active_segment as u32,
            }))
        }

        SessionAction::Commit(text) => {
            let mut r = empty_result(MymeActionType::Commit);
            r.text = into_raw_cstring(text);
            Box::into_raw(Box::new(r))
        }

        SessionAction::Cancel => Box::into_raw(Box::new(empty_result(MymeActionType::Cancel))),
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Creates a new [`MymeContext`] that wraps a fresh [`Session`] and a
/// [`SimpleDictionary`] loaded from `dict_path`.
///
/// # Parameters
///
/// - `dict_path`: null-terminated UTF-8 path to an SKK-format dictionary file.
///   Pass an empty string (`""`) or a path to a non-existent file to use an
///   empty dictionary (the session will still work; conversions will simply
///   return no candidates).
///
/// # Return value
///
/// Returns a non-null `*mut MymeContext` on success.  Returns `null` if:
/// - `dict_path` is a null pointer.
/// - `dict_path` is not valid UTF-8.
/// - The dictionary file cannot be read or parsed.
///
/// The caller is responsible for eventually passing the returned pointer to
/// [`myme_context_destroy`].
#[no_mangle]
pub extern "C" fn myme_context_new(dict_path: *const c_char) -> *mut MymeContext {
    // SAFETY: we check for null before dereferencing.
    if dict_path.is_null() {
        return std::ptr::null_mut();
    }

    // SAFETY: dict_path is non-null; the caller guarantees it is a valid,
    // null-terminated C string for the duration of this call.
    let path_str = match unsafe { CStr::from_ptr(dict_path) }.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };

    let dictionary = if path_str.is_empty() {
        // Convenience: an empty path means "start with an empty dictionary".
        match SimpleDictionary::load_from_skk_text("") {
            Ok(d) => d,
            Err(_) => return std::ptr::null_mut(),
        }
    } else {
        match SimpleDictionary::load_from_file(Path::new(path_str)) {
            Ok(d) => d,
            Err(_) => return std::ptr::null_mut(),
        }
    };

    let ctx = Box::new(MymeContext {
        session: Session::new(),
        dictionary: Box::new(dictionary),
        learning: None,
    });

    Box::into_raw(ctx)
}

/// Creates a new [`MymeContext`] with both a system dictionary and an
/// optional user dictionary.
///
/// # Parameters
///
/// - `dict_path`: path to the system SKK dictionary (same as [`myme_context_new`]).
/// - `user_dict_path`: path to the user SKK dictionary, or null to skip.
///
/// The user dictionary is loaded from `user_dict_path` if non-null.
/// User entries receive a score boost so they rank above system entries.
#[no_mangle]
pub extern "C" fn myme_context_new_with_user_dict(
    dict_path: *const c_char,
    user_dict_path: *const c_char,
) -> *mut MymeContext {
    if dict_path.is_null() {
        return std::ptr::null_mut();
    }

    let path_str = match unsafe { CStr::from_ptr(dict_path) }.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };

    let system = if path_str.is_empty() {
        match SimpleDictionary::load_from_skk_text("") {
            Ok(d) => d,
            Err(_) => return std::ptr::null_mut(),
        }
    } else {
        match SimpleDictionary::load_from_file(Path::new(path_str)) {
            Ok(d) => d,
            Err(_) => return std::ptr::null_mut(),
        }
    };

    let user = if !user_dict_path.is_null() {
        match unsafe { CStr::from_ptr(user_dict_path) }.to_str() {
            Ok(s) if !s.is_empty() => Some(UserDictionary::load(Path::new(s))),
            _ => None,
        }
    } else {
        None
    };

    let dictionary: Box<dyn DictionaryLookup> = Box::new(CompositeDictionary::new(system, user));

    // Set up learning store at ~/Library/Application Support/myme/learning.tsv
    let learning = dirs_learning_path().map(|p| LearningStore::load(&p));

    Box::into_raw(Box::new(MymeContext {
        session: Session::new(),
        dictionary,
        learning,
    }))
}

/// Destroys a [`MymeContext`] previously created by [`myme_context_new`] and
/// frees all associated memory.
///
/// # Safety
///
/// - `ctx` must be non-null and must have been obtained from
///   [`myme_context_new`].
/// - After this call, `ctx` is a dangling pointer; the caller must not use it.
/// - Calling this function more than once with the same pointer is undefined
///   behaviour.
#[no_mangle]
pub extern "C" fn myme_context_destroy(ctx: *mut MymeContext) {
    if ctx.is_null() {
        // Nothing to do; treat as a no-op for caller convenience.
        return;
    }
    // SAFETY: ctx is non-null and was produced by Box::into_raw in
    // myme_context_new.  We reconstruct the Box and let it drop normally,
    // which deallocates all owned memory inside MymeContext.
    unsafe { drop(Box::from_raw(ctx)) };
}

// ---------------------------------------------------------------------------
// Input handling
// ---------------------------------------------------------------------------

/// Processes a single key event and returns the action the IME client must
/// perform.
///
/// # Parameters
///
/// - `ctx`: a non-null context pointer obtained from [`myme_context_new`].
/// - `key_type`: the kind of key event.
/// - `character`: for [`MymeKeyType::Character`], the Unicode codepoint of the
///   character (e.g. `'a'` = 97).  For [`MymeKeyType::Number`], the digit
///   value 1–9.  Ignored for all other key types.
///
/// # Return value
///
/// Returns a heap-allocated [`MymeResult`] that the caller owns and **must**
/// free with [`myme_result_free`].  Returns `null` if `ctx` is null or if
/// `character` encodes an invalid Unicode codepoint for a `Character` event.
#[no_mangle]
pub extern "C" fn myme_handle_key(
    ctx: *mut MymeContext,
    key_type: MymeKeyType,
    character: u32,
) -> *mut MymeResult {
    // SAFETY: we check for null before dereferencing.
    if ctx.is_null() {
        return std::ptr::null_mut();
    }

    // SAFETY: ctx is non-null and was produced by myme_context_new.  We have
    // exclusive access because the caller is responsible for synchronisation.
    let ctx = unsafe { &mut *ctx };

    let key_event = match key_type {
        MymeKeyType::Character => {
            // Validate the Unicode codepoint before converting.
            match char::from_u32(character) {
                Some(ch) => KeyEvent::Character(ch),
                None => return std::ptr::null_mut(),
            }
        }
        MymeKeyType::Space => KeyEvent::Space,
        MymeKeyType::Enter => KeyEvent::Enter,
        MymeKeyType::Backspace => KeyEvent::Backspace,
        MymeKeyType::Escape => KeyEvent::Escape,
        MymeKeyType::ArrowUp => KeyEvent::ArrowUp,
        MymeKeyType::ArrowDown => KeyEvent::ArrowDown,
        MymeKeyType::Number => {
            // Clamp to the valid range 1–9 that KeyEvent::Number accepts.
            let digit = character.min(9) as u8;
            KeyEvent::Number(digit.max(1))
        }
        MymeKeyType::ArrowLeft => KeyEvent::ArrowLeft,
        MymeKeyType::ArrowRight => KeyEvent::ArrowRight,
    };

    let action = ctx.session.handle_key(key_event, &*ctx.dictionary, ctx.learning.as_mut());
    action_to_result(action)
}

/// Frees a [`MymeResult`] previously returned by [`myme_handle_key`].
///
/// # Safety
///
/// - `result` must be non-null and must have been obtained from
///   [`myme_handle_key`].
/// - After this call, `result` is a dangling pointer; the caller must not use
///   it.
/// - Calling this function more than once with the same pointer is undefined
///   behaviour.
#[no_mangle]
pub extern "C" fn myme_result_free(result: *mut MymeResult) {
    if result.is_null() {
        return;
    }

    // SAFETY: result is non-null and was produced by Box::into_raw in
    // action_to_result.  We reconstruct the MymeResult so we can access its
    // fields before the Box drops them.
    let result_box = unsafe { Box::from_raw(result) };

    // Free the text and pending_romaji strings.  These are always non-null
    // (we always write at least an empty CString for them).
    //
    // SAFETY: text and pending_romaji were produced by CString::into_raw in
    // into_raw_cstring and have not been freed yet.
    unsafe {
        free_cstring(result_box.text as *mut c_char);
        free_cstring(result_box.pending_romaji as *mut c_char);
    }

    // Free the candidates array if one was allocated.
    if !result_box.candidates.is_null() && result_box.candidate_count > 0 {
        let count = result_box.candidate_count as usize;
        unsafe {
            let slice = std::slice::from_raw_parts_mut(
                result_box.candidates as *mut *const c_char,
                count,
            );
            for ptr in slice.iter() {
                if !ptr.is_null() {
                    free_cstring(*ptr as *mut c_char);
                }
            }
            drop(Box::from_raw(slice as *mut [*const c_char]));
        }
    }

    // Free the segment_surfaces array if one was allocated.
    if !result_box.segment_surfaces.is_null() && result_box.segment_count > 0 {
        let count = result_box.segment_count as usize;
        unsafe {
            let slice = std::slice::from_raw_parts_mut(
                result_box.segment_surfaces as *mut *const c_char,
                count,
            );
            for ptr in slice.iter() {
                if !ptr.is_null() {
                    free_cstring(*ptr as *mut c_char);
                }
            }
            drop(Box::from_raw(slice as *mut [*const c_char]));
        }
    }

    // result_box is dropped here, freeing the MymeResult allocation itself.
}

// ---------------------------------------------------------------------------
// State query
// ---------------------------------------------------------------------------

/// Returns the current state of the IME session inside `ctx`.
///
/// Returns [`MymeState::Idle`] if `ctx` is null (safe fallback).
#[no_mangle]
pub extern "C" fn myme_get_state(ctx: *const MymeContext) -> MymeState {
    if ctx.is_null() {
        return MymeState::Idle;
    }

    // SAFETY: ctx is non-null and was produced by myme_context_new.  We
    // acquire a shared reference; the caller guarantees no concurrent mutation.
    let ctx = unsafe { &*ctx };

    match ctx.session.state() {
        SessionState::Idle => MymeState::Idle,
        SessionState::Composing => MymeState::Composing,
        SessionState::Converting => MymeState::Converting,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Creates a context backed by an empty dictionary (path = "").
    fn new_empty_ctx() -> *mut MymeContext {
        let path = CString::new("").unwrap();
        let ctx = myme_context_new(path.as_ptr());
        assert!(!ctx.is_null(), "myme_context_new should succeed with empty path");
        ctx
    }

    /// Reads back a `*const c_char` as a Rust `&str`.  The pointer must remain
    /// valid for the duration of the borrow.
    ///
    /// # Safety
    ///
    /// `ptr` must be a non-null, null-terminated UTF-8 C string.
    unsafe fn cstr_to_str(ptr: *const c_char) -> &'static str {
        unsafe { CStr::from_ptr(ptr).to_str().expect("FFI string is not valid UTF-8") }
    }

    // -----------------------------------------------------------------------
    // Lifecycle tests
    // -----------------------------------------------------------------------

    #[test]
    fn context_new_and_destroy() {
        let ctx = new_empty_ctx();
        myme_context_destroy(ctx);
        // If we reach here without panicking or asan-reporting, the test passes.
    }

    #[test]
    fn context_new_null_path_returns_null() {
        let ctx = myme_context_new(std::ptr::null());
        assert!(ctx.is_null());
    }

    #[test]
    fn context_destroy_null_is_noop() {
        // Must not panic or crash.
        myme_context_destroy(std::ptr::null_mut());
    }

    // -----------------------------------------------------------------------
    // State tests
    // -----------------------------------------------------------------------

    #[test]
    fn initial_state_is_idle() {
        let ctx = new_empty_ctx();
        let state = myme_get_state(ctx);
        assert!(matches!(state, MymeState::Idle));
        myme_context_destroy(ctx);
    }

    #[test]
    fn get_state_null_returns_idle() {
        let state = myme_get_state(std::ptr::null());
        assert!(matches!(state, MymeState::Idle));
    }

    // -----------------------------------------------------------------------
    // handle_key: null safety
    // -----------------------------------------------------------------------

    #[test]
    fn handle_key_null_ctx_returns_null() {
        let result = myme_handle_key(std::ptr::null_mut(), MymeKeyType::Space, 0);
        assert!(result.is_null());
    }

    #[test]
    fn handle_key_invalid_codepoint_returns_null() {
        let ctx = new_empty_ctx();
        // 0xFFFFFFFF is not a valid Unicode codepoint.
        let result = myme_handle_key(ctx, MymeKeyType::Character, 0xFFFF_FFFF);
        assert!(result.is_null());
        myme_context_destroy(ctx);
    }

    // -----------------------------------------------------------------------
    // handle_key: basic composing round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn handle_key_character_returns_update_preedit() {
        let ctx = new_empty_ctx();

        // Feed 'a' (U+0061) → should produce UpdatePreedit { text: "あ", ... }
        let result = myme_handle_key(ctx, MymeKeyType::Character, 'a' as u32);
        assert!(!result.is_null());

        let r = unsafe { &*result };
        assert!(matches!(r.action_type, MymeActionType::UpdatePreedit));
        // SAFETY: text and pending_romaji are always non-null.
        let text = unsafe { cstr_to_str(r.text) };
        assert_eq!(text, "あ", "typing 'a' should produce 'あ'");
        let pending = unsafe { cstr_to_str(r.pending_romaji) };
        assert_eq!(pending, "", "no pending romaji after 'a'");

        myme_result_free(result);

        assert!(matches!(myme_get_state(ctx), MymeState::Composing));
        myme_context_destroy(ctx);
    }

    #[test]
    fn handle_key_pending_romaji_is_populated() {
        let ctx = new_empty_ctx();

        // 'k' alone is pending romaji.
        let result = myme_handle_key(ctx, MymeKeyType::Character, 'k' as u32);
        assert!(!result.is_null());
        let r = unsafe { &*result };
        assert!(matches!(r.action_type, MymeActionType::UpdatePreedit));
        let pending = unsafe { cstr_to_str(r.pending_romaji) };
        assert_eq!(pending, "k");

        myme_result_free(result);
        myme_context_destroy(ctx);
    }

    #[test]
    fn handle_key_enter_commits_kana() {
        let ctx = new_empty_ctx();

        // Type 'k', 'a' → "か", then Enter → Commit("か")
        myme_result_free(myme_handle_key(ctx, MymeKeyType::Character, 'k' as u32));
        myme_result_free(myme_handle_key(ctx, MymeKeyType::Character, 'a' as u32));

        let result = myme_handle_key(ctx, MymeKeyType::Enter, 0);
        assert!(!result.is_null());
        let r = unsafe { &*result };
        assert!(matches!(r.action_type, MymeActionType::Commit));
        let text = unsafe { cstr_to_str(r.text) };
        assert_eq!(text, "か");

        myme_result_free(result);
        assert!(matches!(myme_get_state(ctx), MymeState::Idle));
        myme_context_destroy(ctx);
    }

    #[test]
    fn handle_key_escape_cancels() {
        let ctx = new_empty_ctx();

        myme_result_free(myme_handle_key(ctx, MymeKeyType::Character, 'a' as u32));
        let result = myme_handle_key(ctx, MymeKeyType::Escape, 0);
        assert!(!result.is_null());
        let r = unsafe { &*result };
        assert!(matches!(r.action_type, MymeActionType::Cancel));

        myme_result_free(result);
        assert!(matches!(myme_get_state(ctx), MymeState::Idle));
        myme_context_destroy(ctx);
    }

    // -----------------------------------------------------------------------
    // handle_key: candidate window via real SKK dictionary
    // -----------------------------------------------------------------------

    /// Creates a context backed by an in-memory SKK dictionary by writing a
    /// temporary file.
    fn new_ctx_with_skk(skk: &str) -> *mut MymeContext {
        use std::io::Write as IoWrite;
        let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
        tmp.write_all(skk.as_bytes()).expect("write SKK");
        tmp.flush().expect("flush");
        let path = CString::new(tmp.path().to_str().expect("path utf-8")).unwrap();
        // Keep tmp alive until after myme_context_new returns so the file is
        // not deleted before it is read.
        let ctx = myme_context_new(path.as_ptr());
        // tmp is dropped here, but the dictionary is already loaded into RAM.
        assert!(!ctx.is_null());
        ctx
    }

    #[test]
    fn handle_key_space_shows_candidates() {
        let ctx = new_ctx_with_skk("かな /仮名/カナ/\n");

        // Type 'k','a','n','a' → "かな"
        for ch in ['k', 'a', 'n', 'a'] {
            myme_result_free(myme_handle_key(ctx, MymeKeyType::Character, ch as u32));
        }

        // Press Space → ShowCandidates
        let result = myme_handle_key(ctx, MymeKeyType::Space, 0);
        assert!(!result.is_null());
        let r = unsafe { &*result };
        assert!(matches!(r.action_type, MymeActionType::ShowCandidates));
        assert_eq!(r.candidate_count, 2);
        assert!(!r.candidates.is_null());

        // Verify first candidate surface.
        let first = unsafe { cstr_to_str(*r.candidates) };
        assert_eq!(first, "仮名");

        myme_result_free(result);
        assert!(matches!(myme_get_state(ctx), MymeState::Converting));
        myme_context_destroy(ctx);
    }

    #[test]
    fn handle_key_enter_commits_selected_candidate() {
        let ctx = new_ctx_with_skk("かな /仮名/カナ/\n");

        for ch in ['k', 'a', 'n', 'a'] {
            myme_result_free(myme_handle_key(ctx, MymeKeyType::Character, ch as u32));
        }
        myme_result_free(myme_handle_key(ctx, MymeKeyType::Space, 0));

        let result = myme_handle_key(ctx, MymeKeyType::Enter, 0);
        assert!(!result.is_null());
        let r = unsafe { &*result };
        assert!(matches!(r.action_type, MymeActionType::Commit));
        let text = unsafe { cstr_to_str(r.text) };
        assert_eq!(text, "仮名");

        myme_result_free(result);
        assert!(matches!(myme_get_state(ctx), MymeState::Idle));
        myme_context_destroy(ctx);
    }

    #[test]
    fn handle_key_number_selects_candidate() {
        let ctx = new_ctx_with_skk("かな /仮名/カナ/\n");

        for ch in ['k', 'a', 'n', 'a'] {
            myme_result_free(myme_handle_key(ctx, MymeKeyType::Character, ch as u32));
        }
        myme_result_free(myme_handle_key(ctx, MymeKeyType::Space, 0));

        // Press '2' → commit second candidate "カナ"
        let result = myme_handle_key(ctx, MymeKeyType::Number, 2);
        assert!(!result.is_null());
        let r = unsafe { &*result };
        assert!(matches!(r.action_type, MymeActionType::Commit));
        let text = unsafe { cstr_to_str(r.text) };
        assert_eq!(text, "カナ");

        myme_result_free(result);
        myme_context_destroy(ctx);
    }

    // -----------------------------------------------------------------------
    // result_free: null is a no-op
    // -----------------------------------------------------------------------

    #[test]
    fn result_free_null_is_noop() {
        myme_result_free(std::ptr::null_mut());
    }

    // -----------------------------------------------------------------------
    // Noop action
    // -----------------------------------------------------------------------

    #[test]
    fn noop_result_is_returned_for_space_in_idle() {
        let ctx = new_empty_ctx();
        let result = myme_handle_key(ctx, MymeKeyType::Space, 0);
        assert!(!result.is_null());
        let r = unsafe { &*result };
        assert!(matches!(r.action_type, MymeActionType::Noop));
        myme_result_free(result);
        myme_context_destroy(ctx);
    }
}
