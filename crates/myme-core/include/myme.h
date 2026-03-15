/**
 * myme.h — C API for the myme Japanese IME engine.
 *
 * This header declares the C-ABI surface exported by libmyme_core
 * (staticlib: libmyme_core.a / cdylib: libmyme_core.dylib or .so).
 *
 * ## Ownership rules
 *
 * - MymeContext: created by myme_context_new(), destroyed by
 *   myme_context_destroy().  The caller owns the pointer.
 *
 * - MymeResult: created by myme_handle_key(), destroyed by
 *   myme_result_free().  The caller owns the pointer.  All pointer
 *   fields inside MymeResult (text, pending_romaji, candidates,
 *   segment_surfaces) are owned by the MymeResult; they are freed
 *   automatically when myme_result_free() is called.  Do NOT free
 *   any inner pointer separately.
 *
 * ## Thread safety
 *
 * A MymeContext must not be accessed from multiple threads simultaneously
 * without external synchronisation.  Different MymeContext instances may
 * be used concurrently on different threads without additional locking.
 *
 * ## Null-pointer handling
 *
 * All functions that receive a pointer check for null and return a safe
 * default (null pointer or MYME_STATE_IDLE) rather than crashing.
 */

#ifndef MYME_H
#define MYME_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* -------------------------------------------------------------------------
 * Opaque context type
 * ---------------------------------------------------------------------- */

/**
 * Opaque handle to a myme IME context.
 *
 * Holds a live input session, its backing dictionary, and learning store.
 * Obtain one with myme_context_new() and release it with
 * myme_context_destroy().
 */
typedef struct MymeContext MymeContext;

/* -------------------------------------------------------------------------
 * Enumerations
 * ---------------------------------------------------------------------- */

/**
 * Key-event discriminant passed to myme_handle_key().
 *
 * When key_type is MYME_KEY_CHARACTER, the `character` parameter must
 * contain the Unicode codepoint of the character.
 *
 * When key_type is MYME_KEY_NUMBER, the `character` parameter must
 * contain the digit value (1–9).
 *
 * For all other key types, the `character` parameter is ignored.
 */
typedef enum MymeKeyType {
    MYME_KEY_CHARACTER   = 0, /**< Printable character; see `character` param. */
    MYME_KEY_SPACE       = 1, /**< Space bar. */
    MYME_KEY_ENTER       = 2, /**< Return / Enter. */
    MYME_KEY_BACKSPACE   = 3, /**< Backspace / delete-backward. */
    MYME_KEY_ESCAPE      = 4, /**< Escape. */
    MYME_KEY_ARROW_UP    = 5, /**< Up-arrow. */
    MYME_KEY_ARROW_DOWN  = 6, /**< Down-arrow. */
    MYME_KEY_NUMBER      = 7, /**< Digit 1–9; see `character` param. */
    MYME_KEY_ARROW_LEFT  = 8, /**< Left-arrow (segment navigation). */
    MYME_KEY_ARROW_RIGHT = 9, /**< Right-arrow (segment navigation). */
} MymeKeyType;

/**
 * Discriminant describing what the IME client must do after a key event.
 *
 * Inspect MymeResult::action_type to determine which fields of MymeResult
 * are populated.
 */
typedef enum MymeActionType {
    /** Nothing changed that requires a UI update. */
    MYME_ACTION_NOOP             = 0,
    /** Preedit string changed; re-render underlined in-progress text.
     *  MymeResult::text is the confirmed kana portion;
     *  MymeResult::pending_romaji is the trailing unresolved romaji. */
    MYME_ACTION_UPDATE_PREEDIT   = 1,
    /** Candidate window should be shown or refreshed.
     *  MymeResult::text is the kana preedit being converted.
     *  MymeResult::candidates, ::candidate_count, ::selected_index are set.
     *  MymeResult::segment_surfaces, ::segment_count, ::active_segment are set. */
    MYME_ACTION_SHOW_CANDIDATES  = 2,
    /** Insert MymeResult::text into the document; close preedit. */
    MYME_ACTION_COMMIT           = 3,
    /** Discard the preedit and close any open candidate window. */
    MYME_ACTION_CANCEL           = 4,
} MymeActionType;

/**
 * Current phase of the input session.
 */
typedef enum MymeState {
    MYME_STATE_IDLE       = 0, /**< No active preedit. */
    MYME_STATE_COMPOSING  = 1, /**< Building kana from romaji input. */
    MYME_STATE_CONVERTING = 2, /**< Dictionary lookup performed; candidate window open. */
} MymeState;

/* -------------------------------------------------------------------------
 * Result struct
 * ---------------------------------------------------------------------- */

/**
 * Heap-allocated result returned by myme_handle_key().
 *
 * The caller owns this struct and must release it with myme_result_free().
 * Inner pointers (text, pending_romaji, candidates, segment_surfaces) must
 * NOT be freed individually; myme_result_free() handles all deallocation.
 *
 * All string pointers are guaranteed non-null and null-terminated UTF-8.
 * The candidates pointer is null when candidate_count is 0.
 * The segment_surfaces pointer is null when segment_count is 0.
 */
typedef struct MymeResult {
    /** What the IME client must do. */
    MymeActionType action_type;

    /**
     * Null-terminated UTF-8 string.  Semantics depend on action_type:
     *   MYME_ACTION_COMMIT         — text to insert into the document.
     *   MYME_ACTION_UPDATE_PREEDIT — confirmed kana portion of the preedit.
     *   MYME_ACTION_SHOW_CANDIDATES — kana string being converted.
     *   MYME_ACTION_NOOP / CANCEL  — empty string.
     *
     * Never null.
     */
    const char *text;

    /**
     * Null-terminated UTF-8 string containing the not-yet-resolved romaji
     * suffix (e.g. "sh").  Non-empty only when action_type is
     * MYME_ACTION_UPDATE_PREEDIT.  Never null.
     */
    const char *pending_romaji;

    /**
     * Array of `candidate_count` null-terminated UTF-8 strings, each the
     * surface form of one conversion candidate for the active segment
     * (best candidate first).
     * Null when candidate_count is 0.
     */
    const char * const *candidates;

    /** Number of elements in the candidates array. */
    uint32_t candidate_count;

    /**
     * Zero-based index of the currently highlighted candidate.  Meaningful
     * only when action_type is MYME_ACTION_SHOW_CANDIDATES.
     */
    uint32_t selected_index;

    /**
     * Array of `segment_count` null-terminated UTF-8 strings, each the
     * currently selected surface form for one conversion segment.
     * Null when segment_count is 0 (backward compat with non-converting states).
     * Only populated when action_type is MYME_ACTION_SHOW_CANDIDATES.
     */
    const char * const *segment_surfaces;

    /** Number of conversion segments.  0 when not converting. */
    uint32_t segment_count;

    /** Zero-based index of the active (focused) segment. */
    uint32_t active_segment;
} MymeResult;

/* -------------------------------------------------------------------------
 * Lifecycle
 * ---------------------------------------------------------------------- */

/**
 * Creates a new MymeContext backed by the SKK dictionary at dict_path.
 *
 * @param dict_path  Null-terminated UTF-8 path to an SKK-format dictionary
 *                   file.  Pass an empty string ("") to use an empty
 *                   dictionary (composing will work; no conversion candidates
 *                   will be returned).
 *
 * @return  A non-null pointer on success.  Returns NULL if dict_path is NULL,
 *          not valid UTF-8, or the file cannot be read or parsed.
 *
 * The caller must eventually call myme_context_destroy() on the returned
 * pointer.
 */
MymeContext *myme_context_new(const char *dict_path);

/**
 * Creates a new MymeContext with both a system dictionary and an optional
 * user dictionary.
 *
 * @param dict_path       System dictionary path (same as myme_context_new).
 * @param user_dict_path  User dictionary path, or NULL to skip.
 *
 * User dictionary entries receive a score boost so they rank above system
 * entries with the same reading.  Learning history is automatically loaded
 * and saved.
 */
MymeContext *myme_context_new_with_user_dict(const char *dict_path,
                                             const char *user_dict_path);

/**
 * Destroys a MymeContext and frees all associated memory.
 *
 * @param ctx  Pointer obtained from myme_context_new().  Passing NULL is
 *             safe (no-op).
 *
 * After this call ctx is a dangling pointer and must not be used.
 */
void myme_context_destroy(MymeContext *ctx);

/* -------------------------------------------------------------------------
 * Input handling
 * ---------------------------------------------------------------------- */

/**
 * Processes a single key event and returns the required client action.
 *
 * @param ctx        Non-null context pointer from myme_context_new().
 * @param key_type   Kind of key event (see MymeKeyType).
 * @param character  Unicode codepoint for MYME_KEY_CHARACTER, digit value
 *                   (1–9) for MYME_KEY_NUMBER, ignored otherwise.
 *
 * @return  Heap-allocated MymeResult the caller owns and must free with
 *          myme_result_free().  Returns NULL if ctx is NULL or character
 *          is an invalid Unicode codepoint for a MYME_KEY_CHARACTER event.
 */
MymeResult *myme_handle_key(MymeContext *ctx, MymeKeyType key_type, uint32_t character);

/**
 * Frees a MymeResult returned by myme_handle_key().
 *
 * This function releases the MymeResult struct and all memory it owns,
 * including the text, pending_romaji, candidates, and segment_surfaces.
 *
 * @param result  Pointer obtained from myme_handle_key().  Passing NULL is
 *                safe (no-op).
 *
 * After this call result is a dangling pointer and must not be used.
 * Do NOT free result or any of its inner pointers with free() or any
 * other allocator.
 */
void myme_result_free(MymeResult *result);

/* -------------------------------------------------------------------------
 * State query
 * ---------------------------------------------------------------------- */

/**
 * Returns the current state of the input session inside ctx.
 *
 * @param ctx  Context pointer from myme_context_new().  Passing NULL
 *             returns MYME_STATE_IDLE.
 *
 * @return  Current MymeState value.
 */
MymeState myme_get_state(const MymeContext *ctx);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* MYME_H */
