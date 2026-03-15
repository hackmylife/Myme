// MymeBridge.swift
// A type-safe Swift wrapper around the C FFI surface declared in myme.h.
//
// Ownership rules mirrored from the C header:
//   - MymeContext is created by myme_context_new() / destroyed by myme_context_destroy().
//   - MymeResult is created by myme_handle_key() / destroyed by myme_result_free().
//   - No inner pointer of MymeResult may be freed individually.

import Foundation

// ---------------------------------------------------------------------------
// Swift representation of the key discriminant
// ---------------------------------------------------------------------------

/// Maps Swift/AppKit key events onto the C enum that myme_handle_key() expects.
enum MymeBridgeKeyType {
    case character(Unicode.Scalar)
    case space
    case enter
    case backspace
    case escape
    case arrowUp
    case arrowDown
    case arrowLeft
    case arrowRight
    case number(UInt8)          // digit 1-9
}

// ---------------------------------------------------------------------------
// Segment info for multi-segment conversion display
// ---------------------------------------------------------------------------

/// Information about a single conversion segment.
struct MymeBridgeSegment {
    /// The surface form currently selected for this segment.
    let surface: String
    /// Whether this is the active (focused) segment.
    let isActive: Bool
}

// ---------------------------------------------------------------------------
// Swift representation of the action the caller must perform
// ---------------------------------------------------------------------------

/// Decoded action returned after each key event.
enum MymeActionResult {
    /// Nothing changed that requires a UI update.
    case noop
    /// Preedit text updated: `kana` is the confirmed kana, `pending` is the
    /// trailing unresolved romaji (e.g. "sh").  Display kana + pending underlined.
    case updatePreedit(kana: String, pending: String)
    /// Show (or refresh) the candidate window.
    /// `segments` contains per-segment surface + active flag.
    /// `candidates` is ordered best-first for the active segment.
    /// `selected` is the zero-based highlighted index in candidates.
    case showCandidates(
        segments: [MymeBridgeSegment],
        activeSegment: Int,
        candidates: [String],
        selected: Int
    )
    /// Commit `text` into the document and close the preedit.
    case commit(text: String)
    /// Discard the preedit and close any open candidate window.
    case cancel
}

// ---------------------------------------------------------------------------
// Bridge class
// ---------------------------------------------------------------------------

/// Wraps a `MymeContext *` and provides a safe Swift interface.
///
/// The Clang importer maps the opaque C typedef `MymeContext *` to
/// `OpaquePointer` in Swift, so we store it as `OpaquePointer?`.
///
/// Not thread-safe: the underlying C context requires external synchronisation
/// (see myme.h §Thread safety).  Confine usage to the main thread.
final class MymeBridge {

    // `MymeContext *` imported as OpaquePointer by the Swift/Clang importer.
    private var context: OpaquePointer?

    // -----------------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------------

    /// Creates a context backed by the SKK dictionary at `dictPath`.
    /// Pass an empty string to use an empty dictionary (composing works, no
    /// conversion candidates will be returned).
    ///
    /// Returns `nil` if `dictPath` cannot be opened / parsed.
    init?(dictPath: String) {
        let ctx = dictPath.withCString { myme_context_new($0) }
        guard let ctx = ctx else {
            NSLog("MymeBridge: myme_context_new returned NULL for path: %@", dictPath)
            return nil
        }
        context = ctx
    }

    /// Creates a context with both a system dictionary and an optional user
    /// dictionary.  Learning history is automatically managed.
    init?(dictPath: String, userDictPath: String?) {
        let ctx: OpaquePointer?
        if let userPath = userDictPath {
            ctx = dictPath.withCString { dp in
                userPath.withCString { up in
                    myme_context_new_with_user_dict(dp, up)
                }
            }
        } else {
            ctx = dictPath.withCString { dp in
                myme_context_new_with_user_dict(dp, nil)
            }
        }
        guard let ctx = ctx else {
            NSLog("MymeBridge: myme_context_new_with_user_dict returned NULL")
            return nil
        }
        context = ctx
    }

    deinit {
        if let ctx = context {
            myme_context_destroy(ctx)
            context = nil
        }
    }

    // -----------------------------------------------------------------------
    // State query
    // -----------------------------------------------------------------------

    /// Current phase of the input session.
    var state: MymeState {
        guard let ctx = context else { return MYME_STATE_IDLE }
        return myme_get_state(ctx)
    }

    // -----------------------------------------------------------------------
    // Input handling
    // -----------------------------------------------------------------------

    /// Processes a single key event and returns the action the UI must perform.
    /// Returns `.noop` if the context is uninitialised or the FFI returns NULL.
    func handleKey(_ key: MymeBridgeKeyType) -> MymeActionResult {
        guard let ctx = context else { return .noop }

        let keyType: MymeKeyType
        let character: UInt32

        switch key {
        case .character(let scalar):
            keyType = MYME_KEY_CHARACTER
            character = scalar.value
        case .space:
            keyType = MYME_KEY_SPACE
            character = 0
        case .enter:
            keyType = MYME_KEY_ENTER
            character = 0
        case .backspace:
            keyType = MYME_KEY_BACKSPACE
            character = 0
        case .escape:
            keyType = MYME_KEY_ESCAPE
            character = 0
        case .arrowUp:
            keyType = MYME_KEY_ARROW_UP
            character = 0
        case .arrowDown:
            keyType = MYME_KEY_ARROW_DOWN
            character = 0
        case .arrowLeft:
            keyType = MYME_KEY_ARROW_LEFT
            character = 0
        case .arrowRight:
            keyType = MYME_KEY_ARROW_RIGHT
            character = 0
        case .number(let digit):
            keyType = MYME_KEY_NUMBER
            character = UInt32(digit)
        }

        guard let resultPtr = myme_handle_key(ctx, keyType, character) else {
            return .noop
        }
        defer { myme_result_free(resultPtr) }

        return decode(resultPtr.pointee)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Decodes a `MymeResult` value (not a pointer) into a `MymeActionResult`.
    /// Must be called before `myme_result_free` releases the inner pointers.
    private func decode(_ result: MymeResult) -> MymeActionResult {
        switch result.action_type {
        case MYME_ACTION_NOOP:
            return .noop

        case MYME_ACTION_UPDATE_PREEDIT:
            let kana    = result.text.map { String(cString: $0) } ?? ""
            let pending = result.pending_romaji.map { String(cString: $0) } ?? ""
            return .updatePreedit(kana: kana, pending: pending)

        case MYME_ACTION_SHOW_CANDIDATES:
            let count = Int(result.candidate_count)
            var candidates: [String] = []
            candidates.reserveCapacity(count)
            if let arrayPtr = result.candidates {
                for i in 0 ..< count {
                    if let cStr = arrayPtr[i] {
                        candidates.append(String(cString: cStr))
                    }
                }
            }

            // Decode segment surfaces.
            let segCount = Int(result.segment_count)
            let activeIdx = Int(result.active_segment)
            var segments: [MymeBridgeSegment] = []
            segments.reserveCapacity(segCount)
            if let segPtr = result.segment_surfaces {
                for i in 0 ..< segCount {
                    if let cStr = segPtr[i] {
                        segments.append(MymeBridgeSegment(
                            surface: String(cString: cStr),
                            isActive: i == activeIdx
                        ))
                    }
                }
            }

            return .showCandidates(
                segments: segments,
                activeSegment: activeIdx,
                candidates: candidates,
                selected: Int(result.selected_index)
            )

        case MYME_ACTION_COMMIT:
            let text = result.text.map { String(cString: $0) } ?? ""
            return .commit(text: text)

        case MYME_ACTION_CANCEL:
            return .cancel

        default:
            NSLog("MymeBridge: unknown action_type %d, treating as noop", result.action_type.rawValue)
            return .noop
        }
    }
}
