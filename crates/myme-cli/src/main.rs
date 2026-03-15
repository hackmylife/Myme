//! `myme` — command-line development harness for the myme IME engine.
//!
//! # Modes
//!
//! ## Batch convert (default)
//! Read lines from stdin, convert romaji to hiragana, print result.
//! ```
//! echo "konnichiha" | myme
//! ```
//!
//! ## Interactive session (`--interactive` / `-i`)
//! Raw-terminal REPL using crossterm.  Feeds keypresses into a `Session` and
//! renders preedit, candidates, and committed text in-place.
//!
//! ## Lookup mode (`--lookup` / `-l`)
//! Read kana from stdin and print numbered dictionary candidates.
//! ```
//! echo "へんかん" | myme --lookup
//! ```

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use myme_core::dictionary::SimpleDictionary;
use myme_core::romaji::convert;
use myme_core::segmenter;
use myme_core::session::{KeyEvent, Session, SessionAction};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Simple flag scan — no clap needed for four short flags.
    let interactive = args.iter().any(|a| a == "--interactive" || a == "-i");
    let lookup = args.iter().any(|a| a == "--lookup" || a == "-l");
    let eval = args.iter().any(|a| a == "--eval" || a == "-e");

    if interactive {
        if let Err(e) = run_interactive() {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    } else if lookup {
        if let Err(e) = run_lookup() {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    } else if eval {
        // Eval mode: --eval <file.jsonl>
        let eval_path = args
            .iter()
            .position(|a| a == "--eval" || a == "-e")
            .and_then(|i| args.get(i + 1))
            .cloned();

        match eval_path {
            Some(path) => {
                if let Err(e) = run_eval(&path) {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
            None => {
                eprintln!("error: --eval requires a JSONL file path argument");
                std::process::exit(1);
            }
        }
    } else {
        if let Err(e) = run_batch() {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Mode 1: Batch convert
// ---------------------------------------------------------------------------

/// Read lines from stdin, convert each from romaji to hiragana, print result.
fn run_batch() -> io::Result<()> {
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        let kana = convert(&line);
        println!("{kana}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Mode 2: Interactive session
// ---------------------------------------------------------------------------

/// Locate the system dictionary file.
///
/// Search order:
/// 1. `<executable-dir>/../../data/dict/system.dict`  (dev layout)
/// 2. `<executable-dir>/data/dict/system.dict`         (installed layout)
/// 3. `data/dict/system.dict`                          (cwd fallback)
fn find_dict_path() -> Option<PathBuf> {
    // Candidate 1: relative to the executable (covers `cargo run` and installs)
    if let Ok(exe) = std::env::current_exe() {
        // In a Cargo workspace the executable sits at
        //   target/debug/myme  or  target/release/myme
        // and the data directory is at the workspace root two levels up from
        // the `target/<profile>/` directory.
        let candidates = [
            exe.parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
                .map(|root| root.join("data/dict/system.dict")),
            exe.parent()
                .map(|p| p.join("data/dict/system.dict")),
        ];
        for candidate in candidates.into_iter().flatten() {
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // Candidate 2: current working directory
    let cwd_path = PathBuf::from("data/dict/system.dict");
    if cwd_path.exists() {
        return Some(cwd_path);
    }

    None
}

/// RAII guard that disables raw terminal mode when dropped.
///
/// This ensures `disable_raw_mode` is called even if `interactive_loop`
/// panics, preventing the terminal from being left in an unusable state.
struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // Ignore errors: we are in a destructor and there is nothing useful
        // we can do if disabling raw mode fails at this point.
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Run the interactive raw-terminal IME session.
fn run_interactive() -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::terminal::enable_raw_mode;

    // ── Load dictionary ──────────────────────────────────────────────────────
    let dict_path = find_dict_path().ok_or(
        "cannot find data/dict/system.dict — \
         run from the project root or install the dictionary alongside the binary",
    )?;

    let dict = SimpleDictionary::load_from_file(&dict_path)
        .map_err(|e| format!("failed to load dictionary: {e}"))?;

    // ── Set up terminal ──────────────────────────────────────────────────────
    enable_raw_mode()?;

    // RawModeGuard ensures disable_raw_mode is called on every exit path,
    // including panics inside interactive_loop.
    let _guard = RawModeGuard;
    let result = interactive_loop(&dict);

    // _guard is dropped here (or on panic), calling disable_raw_mode().

    // Print a final newline so the shell prompt appears on its own line.
    // This runs after the guard drops, so raw mode is already disabled.
    println!();

    result
}

/// The main event loop for interactive mode.
///
/// Separated from `run_interactive` so that raw-mode cleanup always runs
/// regardless of how this function returns.
fn interactive_loop(
    dict: &dyn myme_core::dictionary::DictionaryLookup,
) -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::event::{read as read_event, Event, KeyCode, KeyModifiers};

    let mut session = Session::new();

    // Print a usage hint before entering the loop.
    println!("myme interactive mode — type romaji, Space to convert, Enter to commit.");
    println!("Ctrl+C or Ctrl+D to quit.\r");

    loop {
        let ev = read_event()?;

        let Event::Key(key_ev) = ev else {
            continue;
        };

        // ── Exit on Ctrl+C or Ctrl+D ──────────────────────────────────────
        if key_ev.modifiers.contains(KeyModifiers::CONTROL) {
            match key_ev.code {
                KeyCode::Char('c') | KeyCode::Char('d') => {
                    print!("\r[exit]\r\n");
                    io::stdout().flush()?;
                    break;
                }
                _ => {}
            }
        }

        // ── Map crossterm KeyCode → myme_core KeyEvent ─────────────────────
        let myme_key = match key_ev.code {
            // In crossterm 0.28, space is represented as KeyCode::Char(' ').
            KeyCode::Char(' ') => KeyEvent::Space,
            KeyCode::Char(ch) => {
                // Digits 1–9 are forwarded as Number events when the session is
                // in Converting state so the user can select a candidate directly.
                // In all other contexts they just act as Character events.
                if ch.is_ascii_digit() && ch != '0' {
                    let n = ch as u8 - b'0';
                    KeyEvent::Number(n)
                } else {
                    KeyEvent::Character(ch)
                }
            }
            KeyCode::Enter => KeyEvent::Enter,
            KeyCode::Backspace => KeyEvent::Backspace,
            KeyCode::Esc => KeyEvent::Escape,
            KeyCode::Up => KeyEvent::ArrowUp,
            KeyCode::Down => KeyEvent::ArrowDown,
            KeyCode::Tab => KeyEvent::Space, // Tab also triggers conversion
            KeyCode::Left => KeyEvent::ArrowLeft,
            KeyCode::Right => KeyEvent::ArrowRight,
            KeyCode::F(_) | KeyCode::Home
            | KeyCode::End | KeyCode::PageUp | KeyCode::PageDown | KeyCode::Insert
            | KeyCode::Delete => {
                // Ignored in this harness.
                continue;
            }
            _ => continue,
        };

        // ── Feed key to the session and handle the resulting action ─────────
        let action = session.handle_key(myme_key, dict, None);
        handle_action(&action)?;
    }

    Ok(())
}

/// Render a `SessionAction` to the terminal.
///
/// Uses carriage-return (`\r`) explicitly because the terminal is in raw mode;
/// otherwise the cursor would not return to column 0.
fn handle_action(action: &SessionAction) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    match action {
        SessionAction::Noop => {}

        SessionAction::UpdatePreedit {
            text,
            pending_romaji,
        } => {
            // Overwrite the current line with the updated preedit.
            write!(
                out,
                "\r\x1b[2K[composing] {text}{pending_romaji}"
            )?;
            out.flush()?;
        }

        SessionAction::ShowCandidates {
            segments,
            candidates,
            selected,
            preedit: _,
            ..
        } => {
            // First line: show all segments, highlighting the active one.
            let seg_display: String = segments
                .iter()
                .map(|s| {
                    if s.is_active {
                        format!("[{}]", s.surface)
                    } else {
                        s.surface.clone()
                    }
                })
                .collect();
            write!(out, "\r\x1b[2K[converting] {seg_display}\r\n")?;

            // Print up to 9 numbered candidates, highlighting the selected one.
            for (i, candidate) in candidates.iter().enumerate().take(9) {
                let marker = if i == *selected { ">" } else { " " };
                write!(out, "\r  {marker} {}. {}\r\n", i + 1, candidate.surface)?;
            }

            out.flush()?;
        }

        SessionAction::Commit(text) => {
            write!(out, "\r\x1b[2K[committed] {text}\r\n")?;
            out.flush()?;
        }

        SessionAction::Cancel => {
            write!(out, "\r\x1b[2K[cancelled]\r\n")?;
            out.flush()?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Mode 3: Lookup
// ---------------------------------------------------------------------------

/// Read kana from stdin and print numbered dictionary candidates.
fn run_lookup() -> Result<(), Box<dyn std::error::Error>> {
    use myme_core::dictionary::DictionaryLookup as _;

    let dict_path = find_dict_path().ok_or(
        "cannot find data/dict/system.dict — \
         run from the project root or install the dictionary alongside the binary",
    )?;

    let dict = SimpleDictionary::load_from_file(&dict_path)
        .map_err(|e| format!("failed to load dictionary: {e}"))?;

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let reading = line?;
        let reading = reading.trim().to_string();
        if reading.is_empty() {
            continue;
        }

        let candidates = dict.lookup(&reading);

        if candidates.is_empty() {
            println!("(no candidates for {reading})");
        } else {
            for (i, candidate) in candidates.iter().enumerate() {
                println!("{}. {}", i + 1, candidate.surface);
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Mode 4: Evaluation
// ---------------------------------------------------------------------------

/// A single evaluation test case.
#[derive(serde::Deserialize)]
struct EvalCase {
    /// Kana reading input (e.g. "きょうはいいてんきです").
    input: String,
    /// Expected output after greedy conversion (e.g. "今日はいい天気です").
    expected: String,
}

/// Run evaluation on a JSONL file and report metrics.
fn run_eval(eval_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let dict_path = find_dict_path().ok_or(
        "cannot find data/dict/system.dict — \
         run from the project root or install the dictionary alongside the binary",
    )?;

    let dict = SimpleDictionary::load_from_file(&dict_path)
        .map_err(|e| format!("failed to load dictionary: {e}"))?;

    let text = std::fs::read_to_string(eval_path)
        .map_err(|e| format!("failed to read {eval_path}: {e}"))?;

    let mut total = 0usize;
    let mut exact_match = 0usize;
    let mut total_segments = 0usize;
    let mut correct_segments = 0usize;
    let mut failures: Vec<(String, String, String)> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let case: EvalCase = serde_json::from_str(line)
            .map_err(|e| format!("failed to parse JSONL line: {e}"))?;

        // Segment the input using greedy longest-match.
        let segments = segmenter::segment(&case.input, &dict);

        // Build the converted output by joining each segment's top candidate.
        let actual: String = segments
            .iter()
            .map(|seg| seg.selected_surface())
            .collect();

        total += 1;

        if actual == case.expected {
            exact_match += 1;
        } else {
            failures.push((case.input.clone(), case.expected.clone(), actual.clone()));
        }

        // Per-segment accuracy: compare character-by-character between
        // expected and actual, counting segments that match.
        let expected_chars: Vec<char> = case.expected.chars().collect();
        let mut expected_pos = 0;
        for seg in &segments {
            let seg_surface = seg.selected_surface();
            let seg_chars: Vec<char> = seg_surface.chars().collect();
            let seg_len = seg_chars.len();

            let matches = if expected_pos + seg_len <= expected_chars.len() {
                seg_chars
                    .iter()
                    .zip(&expected_chars[expected_pos..expected_pos + seg_len])
                    .all(|(a, b)| a == b)
            } else {
                false
            };

            total_segments += 1;
            if matches {
                correct_segments += 1;
            }
            expected_pos += seg_len;
        }
    }

    // Report results.
    println!("=== Evaluation Results ===");
    println!("Total cases:       {total}");
    println!(
        "Exact match:       {exact_match}/{total} ({:.1}%)",
        if total > 0 { exact_match as f64 / total as f64 * 100.0 } else { 0.0 }
    );
    println!(
        "Segment accuracy:  {correct_segments}/{total_segments} ({:.1}%)",
        if total_segments > 0 {
            correct_segments as f64 / total_segments as f64 * 100.0
        } else {
            0.0
        }
    );

    if !failures.is_empty() {
        println!("\n--- Failures ---");
        for (input, expected, actual) in &failures {
            println!("  input:    {input}");
            println!("  expected: {expected}");
            println!("  actual:   {actual}");
            println!();
        }
    }

    Ok(())
}
