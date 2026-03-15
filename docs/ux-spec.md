# myme IME — UX Specification (Phase 1)

## Overview

This document defines the Phase 1 interaction model for myme, a macOS Japanese IME. It covers key bindings, state transitions, preedit rendering, and the candidate window. This spec is intentionally scoped to Phase 1 and avoids over-designing future concerns.

---

## 1. Key Bindings

| Key           | Idle        | Composing                          | Converting                        |
|---------------|-------------|------------------------------------|------------------------------------|
| `a`–`z`       | → Composing | Append romaji; update preedit      | (ignored or restart composing)     |
| `Space`       | Pass-through| → Converting; show candidates      | Advance to next candidate (Down)   |
| `Enter`       | Pass-through| Commit kana as-is → Idle           | Commit selected candidate → Idle   |
| `Backspace`   | Pass-through| Delete last preedit character      | Cancel conversion → Composing      |
| `Escape`      | Pass-through| Cancel all; discard preedit → Idle | Cancel conversion → Composing      |
| `Arrow Up`    | Pass-through| (ignored)                          | Select previous candidate          |
| `Arrow Down`  | Pass-through| (ignored)                          | Select next candidate              |
| `Tab`         | Pass-through| (ignored)                          | Select next candidate (same as ↓)  |
| `1`–`9`       | Pass-through| Pass-through                       | Select candidate by number → Idle  |

Notes:
- "Pass-through" means the key event is forwarded to the active application unchanged.
- In Composing, character keys append romaji and the preedit is updated immediately after each keystroke.
- In Converting, alpha keys other than numbers are ignored to avoid accidental disruption of the candidate session.

---

## 2. State Machine

```
          [character key]
 Idle ─────────────────────────────► Composing
  ▲                                      │  ▲
  │  [Enter] commit kana                 │  │
  │◄─────────────────────────────────────┘  │
  │                                         │
  │  [Escape] discard preedit               │ [Escape] cancel conversion
  │◄─────────────────────────────────────┐  │
  │                             [Space]  │  │
  │                        ┌────────────►│  │
  │                         ▼            │  │
  │                      Converting ─────┘  │
  │                         │               │
  │  [Enter] commit selected │               │
  │◄────────────────────────┘               │
  │                                         │
  │  [1-9] commit by number                 │
  │◄──────────────────────── Converting ────┘
```

### States

**Idle**
- No active preedit. The IME is transparent.
- All keys except those that begin romaji input are passed through.

**Composing**
- A preedit buffer is active and displayed inline.
- Romaji is accumulated and converted to kana incrementally (e.g., `k` + `a` → `か`).
- Trailing romaji that has not yet resolved to kana is shown verbatim at the end of the preedit.

**Converting**
- The kana preedit has been submitted for kanji conversion.
- A candidate window is displayed.
- The first candidate is pre-selected and shown in the preedit area with a thick underline.

### Transitions

| # | From       | Trigger                  | To         | Action                                      |
|---|------------|--------------------------|------------|---------------------------------------------|
| 1 | Idle       | Character key (`a`–`z`)  | Composing  | Initialize preedit buffer; append character |
| 2 | Composing  | `Space`                  | Converting | Request candidates; show candidate window   |
| 3 | Converting | `Enter`                  | Idle       | Commit selected candidate; clear preedit    |
| 4 | Converting | Number key `1`–`9`       | Idle       | Commit candidate at that position           |
| 5 | Converting | `Escape`                 | Composing  | Dismiss candidate window; restore kana      |
| 6 | Composing  | `Enter`                  | Idle       | Commit current kana as-is                   |
| 7 | Composing  | `Escape`                 | Idle       | Discard preedit entirely                    |
| 8 | Converting | `Backspace`              | Composing  | Same as Escape (cancel conversion)          |

---

## 3. Preedit Display

The preedit is rendered inline at the cursor position inside the active text field using the macOS Text Input Client API (`setMarkedText`).

### Composing State

- Kana characters are shown with a **single underline**.
- Any trailing romaji that has not yet resolved (e.g., the `k` in `ka` before `a` is pressed) is shown as romaji with the same underline.
- Example: typing `nihon` while `ni` has resolved but `h` is pending → `にh`

```
 ┌────────────────────────────────────┐
 │ text before cursor │ にほん│ text after│
 │                    ‾‾‾‾‾‾         │
 └────────────────────────────────────┘
                       ^ single underline
```

### Converting State

- The selected candidate replaces the preedit region and is shown with a **thick underline**.
- Example: `にほん` converted to `日本`

```
 ┌────────────────────────────────────┐
 │ text before cursor │ 日本 │ text after│
 │                    ══════         │
 └────────────────────────────────────┘
                       ^ thick underline
```

### Attribute Details

| State      | NSAttributedString key       | Value                              |
|------------|------------------------------|------------------------------------|
| Composing  | `NSUnderlineStyleAttributeName` | `.single`                       |
| Converting | `NSUnderlineStyleAttributeName` | `.thick`                        |
| Both       | `NSUnderlineColorAttributeName` | System default (matches text color)|

---

## 4. Candidate Window

### Layout

```
 ┌─────────────────┐
 │ 1. 日本          │  ← selected (highlighted)
 │ 2. 二本          │
 │ 3. ニホン         │
 │ 4. 二ほん         │
 │ 5. にほん         │
 │ 6. …             │
 │ 7. …             │
 │ 8. …             │
 │ 9. …             │
 │ ─────────────── │
 │ 1/3 ▲ ▼         │  ← page indicator
 └─────────────────┘
```

### Behavior

- Displays up to **9 candidates per page**, each labeled `1`–`9`.
- The currently selected candidate is **highlighted** (system selection color).
- `Arrow Down` / `Tab` / `Space` advances the selection downward; wraps to next page.
- `Arrow Up` moves selection upward; wraps to previous page.
- Page indicator shows current page / total pages (e.g., `1/3`).
- Pressing a number key `1`–`9` immediately commits the corresponding candidate and dismisses the window.
- The window is positioned below the preedit cursor, or above if insufficient space below.

### Sizing

- Width: fit to the longest candidate label, minimum 120 pt.
- Height: auto-sized to the number of candidates shown (max 9 rows + footer).
- Font: system UI font at the same size as the active text field (fallback: 13 pt).

---

## 5. Out of Scope (Phase 1)

The following are explicitly deferred to later phases:

- Inline candidate selection (horizontal candidate bar)
- Learning / frequency adjustment based on user selections
- User dictionary
- Partial conversion / segment boundary adjustment (`Shift+Arrow`)
- Katakana / half-width conversion modes
- Preferences UI
- Accessibility (VoiceOver) integration beyond basic preedit announcement
