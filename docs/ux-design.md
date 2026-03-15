# myme IME — Phase 1 Interaction Model

This document is the canonical Phase 1 design reference for myme's interaction model. It defines the state machine, per-state key bindings, preedit rendering, and candidate window. Implementation details (API calls, data structures) belong in separate docs.

---

## 1. State Machine

```
                     ┌─────────────────────────────────────────┐
                     │           printable char                │
                     ▼                                         │
              ┌────────────┐    printable char    ┌────────────┴───┐
  ──────────► │    Idle    │ ──────────────────► │   Composing    │
              └────────────┘                     └────────────────┘
                     ▲                              │  ▲  │  ▲
                     │      Enter (commit kana)     │  │  │  │
                     │ ◄────────────────────────────┘  │  │  │
                     │                                 │  │  │
                     │      Escape (discard preedit)   │  │  │
                     │ ◄───────────────────────────────┘  │  │
                     │                                    │  │
                     │                           Space    │  │ Escape / Backspace
                     │                                    ▼  │ (cancel conversion)
                     │                           ┌────────────────┐
                     │      Enter (commit)        │  Converting   │
                     │ ◄──────────────────────── │               │
                     │                           └────────────────┘
                     │      1-9 (commit by num)          │
                     │ ◄─────────────────────────────────┘
```

### State Descriptions

**Idle**
The IME is transparent. No preedit buffer exists. All keys except romaji starters are passed through to the application unchanged.

**Composing**
A preedit buffer is active and rendered inline. Romaji keystrokes are accumulated and resolved to kana incrementally. Unresolved trailing romaji is shown verbatim at the end of the preedit.

**Converting**
The kana preedit has been submitted for kanji/kana conversion. A candidate window is shown. The first candidate is pre-selected and displayed in the preedit region with a thick underline.

### Transition Table

| # | From       | Trigger                       | To         | Action                                        |
|---|------------|-------------------------------|------------|-----------------------------------------------|
| 1 | Idle       | Printable char (`a`–`z`, etc) | Composing  | Create preedit buffer; append character       |
| 2 | Composing  | `Space`                       | Converting | Request candidates; show candidate window     |
| 3 | Composing  | `Enter`                       | Idle       | Commit current kana preedit as-is             |
| 4 | Composing  | `Escape`                      | Idle       | Discard preedit entirely                      |
| 5 | Converting | `Enter`                       | Idle       | Commit selected candidate; clear preedit      |
| 6 | Converting | `1`–`9`                       | Idle       | Commit candidate at that position             |
| 7 | Converting | `Escape`                      | Composing  | Dismiss candidate window; restore kana preedit|
| 8 | Converting | `Backspace`                   | Composing  | Same as Escape (cancel conversion)            |

---

## 2. Key Bindings per State

### Idle

| Key              | Action                                    |
|------------------|-------------------------------------------|
| `a`–`z`, etc.    | Start composing; initialize preedit buffer |
| All other keys   | Pass through to application               |

### Composing

| Key              | Action                                                   |
|------------------|----------------------------------------------------------|
| `a`–`z`, etc.    | Append to romaji buffer; update kana preedit immediately |
| `Backspace`      | Delete last character from preedit buffer                |
| `Enter`          | Commit current kana preedit as-is; return to Idle        |
| `Space`          | Trigger conversion; move to Converting state             |
| `Escape`         | Discard entire preedit; return to Idle                   |
| Arrow / other    | Pass through (no preedit navigation in Phase 1)          |

### Converting

| Key              | Action                                                         |
|------------------|----------------------------------------------------------------|
| `Space`          | Advance to next candidate (same as Arrow Down)                 |
| `Enter`          | Commit currently selected candidate; return to Idle            |
| `1`–`9`          | Commit candidate at that label position immediately            |
| `Backspace`      | Cancel conversion; return to Composing with kana restored      |
| `Escape`         | Cancel conversion; return to Composing with kana restored      |
| `Arrow Down`     | Move selection to next candidate; scroll page if needed        |
| `Arrow Up`       | Move selection to previous candidate; scroll page if needed    |
| Other keys       | Ignored                                                        |

---

## 3. Preedit Display

The preedit is rendered inline at the cursor using the macOS `setMarkedText` API with `NSAttributedString` underline attributes.

### Composing State

Resolved kana and unresolved trailing romaji are shown together with a single underline.

```
Example: user has typed "niho" — "ni" resolved, "ho" in progress

 ┌──────────────────────────────────────────┐
 │  (existing text) │ にho │ (existing text) │
 │                   ‾‾‾‾‾                  │
 └──────────────────────────────────────────┘
                     ^ single underline
                       kana + pending romaji
```

Romaji that fully resolves replaces itself with kana on the next keystroke:
- `k` → shows `k`
- `ka` → shows `か`
- `kak` → shows `かk`
- `kak` + `u` → shows `かく`

### Converting State

The selected candidate replaces the preedit region and is shown with a thick underline.

```
Example: "にほん" → first candidate "日本" is selected

 ┌──────────────────────────────────────────┐
 │  (existing text) │ 日本 │ (existing text) │
 │                   ════                   │
 └──────────────────────────────────────────┘
                     ^ thick underline
                       selected candidate
```

### NSAttributedString Attribute Reference

| State      | Attribute key                   | Value                            |
|------------|---------------------------------|----------------------------------|
| Composing  | `NSUnderlineStyleAttributeName` | `.single`                        |
| Converting | `NSUnderlineStyleAttributeName` | `.thick`                         |
| Both       | `NSUnderlineColorAttributeName` | System default (matches text)    |

---

## 4. Candidate Window

### Layout

```
 ┌──────────────────────┐
 │ 1. 日本               │  ← selected (highlighted)
 │ 2. 二本               │
 │ 3. ニホン              │
 │ 4. 二ほん              │
 │ 5. にほん              │
 │ 6. …                  │
 │ 7. …                  │
 │ 8. …                  │
 │ 9. …                  │
 │ ──────────────────── │
 │  1/3  ▲ ▼            │  ← page / navigation indicator
 └──────────────────────┘
```

### Behavior

- Displays up to **9 candidates per page**, each labeled `1`–`9`.
- The currently selected candidate is **highlighted** using the system selection color.
- `Space` / `Arrow Down` advances selection; wraps to the next page at the bottom.
- `Arrow Up` moves selection upward; wraps to the previous page at the top.
- Pressing `1`–`9` immediately commits that candidate and closes the window.
- The window is positioned directly below the preedit cursor. If insufficient vertical space exists below, it appears above the cursor instead.

### Sizing

| Property  | Rule                                                   |
|-----------|--------------------------------------------------------|
| Width     | Fit to longest candidate label; minimum 120 pt         |
| Height    | Auto-sized to rows shown (max 9 rows) plus footer row  |
| Font      | System UI font matching the active text field; fallback 13 pt |

---

## 5. Out of Scope (Phase 1)

The following are explicitly deferred:

- Horizontal (inline) candidate bar
- Segment boundary adjustment (`Shift+Arrow`)
- Katakana / half-width conversion modes
- User dictionary and learning / frequency adjustment
- Preferences UI
- Accessibility (VoiceOver) integration beyond basic preedit announcement
- Multi-segment conversion
