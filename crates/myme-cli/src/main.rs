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

    // Simple flag scan — no clap needed for a handful of short flags.
    let interactive = args.iter().any(|a| a == "--interactive" || a == "-i");
    let lookup = args.iter().any(|a| a == "--lookup" || a == "-l");
    let eval = args.iter().any(|a| a == "--eval" || a == "-e");
    let verbose = args.iter().any(|a| a == "--verbose" || a == "-v");

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
        // Eval mode: --eval <file.jsonl> [--report <out.jsonl>] [--tag <tag>]
        let eval_path = args
            .iter()
            .position(|a| a == "--eval" || a == "-e")
            .and_then(|i| args.get(i + 1))
            .cloned();

        let report_path = args
            .iter()
            .position(|a| a == "--report" || a == "-r")
            .and_then(|i| args.get(i + 1))
            .cloned();

        let tag_filter = args
            .iter()
            .position(|a| a == "--tag")
            .and_then(|i| args.get(i + 1))
            .cloned();

        match eval_path {
            Some(path) => {
                if let Err(e) = run_eval(&path, verbose, report_path.as_deref(), tag_filter.as_deref()) {
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

/// A single evaluation test case loaded from a JSONL file.
#[derive(serde::Deserialize)]
struct EvalCase {
    /// Kana reading input (e.g. "きょうはいいてんきです").
    input: String,
    /// Expected output after conversion (e.g. "今日はいい天気です").
    expected: String,
    /// Optional tags for filtering and per-tag reporting.
    #[serde(default)]
    tags: Vec<String>,
    /// Optional human-readable note about the test case (stored for report
    /// completeness; not used in scoring).
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

/// Per-segment detail within a single eval case result.
#[derive(serde::Serialize)]
struct EvalSegmentResult {
    /// The kana reading for this segment.
    reading: String,
    /// The top-1 candidate surface selected by the engine.
    selected: String,
    /// The expected surface for this segment, if it can be determined by
    /// aligning the expected string with the segment boundaries.
    expected_surface: Option<String>,
    /// 1-based rank of the expected surface in the full candidate list.
    /// `None` means the expected surface was not found in any candidate.
    rank: Option<usize>,
    /// Top-5 candidate surfaces for inspection.
    candidates_top5: Vec<String>,
    /// One of: "correct", "rank_miss", "not_found", "segmentation_error".
    status: String,
}

/// Full result for one eval case, suitable for JSONL report output.
#[derive(serde::Serialize)]
struct EvalCaseResult {
    /// Original kana input.
    input: String,
    /// Expected converted output.
    expected: String,
    /// Actual converted output produced by the engine.
    actual: String,
    /// Whether `actual == expected`.
    exact_match: bool,
    /// Per-segment breakdown.
    segments: Vec<EvalSegmentResult>,
    /// Highest-priority error category across all segments, or "correct".
    /// Priority order (worst first): "not_found" > "segmentation_error" > "rank_miss" > "correct".
    error_category: Option<String>,
    /// Tags inherited from the source `EvalCase`.
    tags: Vec<String>,
    /// Same as `exact_match` — top-1 accuracy indicator.
    top1_match: bool,
    /// True if the expected output could be produced by selecting from the
    /// top-3 candidates of each segment.
    top3_match: bool,
    /// True if the expected output could be produced by selecting from the
    /// top-5 candidates of each segment.
    top5_match: bool,
}

/// Aggregated metrics across a set of `EvalCaseResult`s.
struct EvalMetrics {
    total: usize,
    top1: usize,
    top3: usize,
    top5: usize,
    not_found: usize,
    rank_miss: usize,
    /// Per-tag counts: tag → (top1_matches, total_cases).
    per_tag: std::collections::HashMap<String, (usize, usize)>,
}

// ---------------------------------------------------------------------------
// Top-N match logic
// ---------------------------------------------------------------------------

/// Returns `true` if the `expected` string can be reconstructed by picking
/// one candidate from the top-`n` list of each segment, in order.
///
/// The algorithm aligns `expected` character-by-character against segments:
/// for each segment it tries each of the top-`n` candidates and checks whether
/// that candidate's surface matches the next `surface.len()` characters of
/// `expected` at `pos`.  If every segment finds a match, the whole string is
/// constructible from top-N.
fn is_top_n_match(segments: &[myme_core::segmenter::Segment], expected: &str, n: usize) -> bool {
    let expected_chars: Vec<char> = expected.chars().collect();
    let mut pos = 0usize;

    for seg in segments {
        let top_n_surfaces: Vec<&str> = seg
            .candidates
            .iter()
            .take(n)
            .map(|c| c.surface.as_str())
            .collect();

        let matched = top_n_surfaces.iter().any(|surface| {
            let surface_chars: Vec<char> = surface.chars().collect();
            let len = surface_chars.len();
            if pos + len > expected_chars.len() {
                return false;
            }
            surface_chars
                .iter()
                .zip(&expected_chars[pos..pos + len])
                .all(|(a, b)| a == b)
        });

        if !matched {
            return false;
        }

        // Advance pos by the length of whichever top-n candidate matched.
        // We need to find which one matched to know how far to advance.
        // Since any match means the segment is covered, advance by the length
        // of the first matching candidate.
        let advance = top_n_surfaces
            .iter()
            .find_map(|surface| {
                let surface_chars: Vec<char> = surface.chars().collect();
                let len = surface_chars.len();
                if pos + len > expected_chars.len() {
                    return None;
                }
                let ok = surface_chars
                    .iter()
                    .zip(&expected_chars[pos..pos + len])
                    .all(|(a, b)| a == b);
                if ok { Some(len) } else { None }
            })
            .unwrap_or(0);

        pos += advance;
    }

    // All segments matched and we consumed the entire expected string.
    pos == expected_chars.len()
}

// ---------------------------------------------------------------------------
// Per-case evaluation
// ---------------------------------------------------------------------------

/// Evaluate one `EvalCase` against the dictionary and return a full result.
fn evaluate_case(
    case: &EvalCase,
    dict: &dyn myme_core::dictionary::DictionaryLookup,
) -> EvalCaseResult {
    let segments = segmenter::segment(&case.input, dict);

    // Build the actual output from top-1 candidates.
    let actual: String = segments.iter().map(|seg| seg.selected_surface()).collect();

    let exact_match = actual == case.expected;
    let top1_match = exact_match;
    let top3_match = is_top_n_match(&segments, &case.expected, 3);
    let top5_match = is_top_n_match(&segments, &case.expected, 5);

    // Align expected string against segments to determine per-segment
    // expected surfaces.
    let expected_chars: Vec<char> = case.expected.chars().collect();
    let mut exp_pos = 0usize;

    let mut segment_results: Vec<EvalSegmentResult> = Vec::with_capacity(segments.len());

    // Track overall error category across all segments.
    // Priority (worst first): not_found > segmentation_error > rank_miss > correct
    let mut worst_error: u8 = 0; // 0=correct, 1=rank_miss, 2=segmentation_error, 3=not_found

    for seg in &segments {
        let selected = seg.selected_surface().to_string();
        let top5: Vec<String> = seg
            .candidates
            .iter()
            .take(5)
            .map(|c| c.surface.clone())
            .collect();

        // Determine the expected surface for this segment by slicing the
        // expected string at the current position.  We try every candidate
        // length to find one that fits; if none match we mark as
        // segmentation_error.
        //
        // Strategy: try the selected surface length first (most likely to
        // align correctly), then try all other candidate lengths.
        let seg_surface_len_chars = seg.selected_surface().chars().count();
        let expected_surface_opt: Option<String> = {
            // Collect all unique surface lengths from the segment's candidates,
            // starting with the selected surface length for alignment preference.
            let mut lengths: Vec<usize> = seg
                .candidates
                .iter()
                .map(|c| c.surface.chars().count())
                .collect();
            lengths.sort_unstable();
            lengths.dedup();
            // Move the selected surface length to the front.
            if let Some(idx) = lengths.iter().position(|&l| l == seg_surface_len_chars) {
                lengths.remove(idx);
                lengths.insert(0, seg_surface_len_chars);
            }

            lengths.into_iter().find_map(|len| {
                if exp_pos + len <= expected_chars.len() {
                    Some(expected_chars[exp_pos..exp_pos + len].iter().collect::<String>())
                } else {
                    None
                }
            })
        };

        let (rank, status) = match &expected_surface_opt {
            None => {
                // Cannot align — segmentation boundary mismatch.
                (None, "segmentation_error".to_string())
            }
            Some(exp_surf) => {
                if *exp_surf == selected {
                    // Top-1 correct.
                    (Some(1usize), "correct".to_string())
                } else {
                    // Check if expected surface appears in full candidate list.
                    let rank_opt = seg
                        .candidates
                        .iter()
                        .position(|c| &c.surface == exp_surf)
                        .map(|i| i + 1); // convert 0-based to 1-based

                    match rank_opt {
                        Some(r) => (Some(r), "rank_miss".to_string()),
                        None => (None, "not_found".to_string()),
                    }
                }
            }
        };

        // Update worst error priority.
        let priority: u8 = match status.as_str() {
            "not_found" => 3,
            "segmentation_error" => 2,
            "rank_miss" => 1,
            _ => 0,
        };
        if priority > worst_error {
            worst_error = priority;
        }

        // Advance exp_pos by the length of the expected surface if we found one,
        // otherwise advance by the selected surface length (best effort).
        let advance_len = expected_surface_opt
            .as_ref()
            .map(|s| s.chars().count())
            .unwrap_or_else(|| seg.selected_surface().chars().count());
        exp_pos += advance_len;

        segment_results.push(EvalSegmentResult {
            reading: seg.reading.clone(),
            selected,
            expected_surface: expected_surface_opt,
            rank,
            candidates_top5: top5,
            status,
        });
    }

    let error_category = if exact_match {
        Some("correct".to_string())
    } else {
        Some(match worst_error {
            3 => "not_found",
            2 => "segmentation_error",
            1 => "rank_miss",
            _ => "correct",
        }.to_string())
    };

    EvalCaseResult {
        input: case.input.clone(),
        expected: case.expected.clone(),
        actual,
        exact_match,
        segments: segment_results,
        error_category,
        tags: case.tags.clone(),
        top1_match,
        top3_match,
        top5_match,
    }
}

// ---------------------------------------------------------------------------
// Metrics aggregation
// ---------------------------------------------------------------------------

/// Compute aggregate metrics from a slice of case results.
fn compute_metrics(results: &[EvalCaseResult]) -> EvalMetrics {
    let mut metrics = EvalMetrics {
        total: results.len(),
        top1: 0,
        top3: 0,
        top5: 0,
        not_found: 0,
        rank_miss: 0,
        per_tag: std::collections::HashMap::new(),
    };

    for r in results {
        if r.top1_match {
            metrics.top1 += 1;
        }
        if r.top3_match {
            metrics.top3 += 1;
        }
        if r.top5_match {
            metrics.top5 += 1;
        }
        if r.error_category.as_deref() == Some("not_found") {
            metrics.not_found += 1;
        }
        if r.error_category.as_deref() == Some("rank_miss") {
            metrics.rank_miss += 1;
        }

        // Per-tag counts.
        for tag in &r.tags {
            let entry = metrics.per_tag.entry(tag.clone()).or_insert((0, 0));
            entry.1 += 1; // total
            if r.top1_match {
                entry.0 += 1; // top1
            }
        }
    }

    metrics
}

// ---------------------------------------------------------------------------
// Summary printing
// ---------------------------------------------------------------------------

fn pct(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64 * 100.0
    }
}

fn print_summary(eval_path: &str, metrics: &EvalMetrics, results: &[EvalCaseResult], verbose: bool) {
    // Extract just the file name for a cleaner header.
    let file_name = std::path::Path::new(eval_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(eval_path);

    println!("=== Evaluation Results: {file_name} ===");
    println!("Cases:           {}", metrics.total);
    println!(
        "Top-1 accuracy:  {:.1}% ({}/{})",
        pct(metrics.top1, metrics.total),
        metrics.top1,
        metrics.total
    );
    println!(
        "Top-3 accuracy:  {:.1}% ({}/{})",
        pct(metrics.top3, metrics.total),
        metrics.top3,
        metrics.total
    );
    println!(
        "Top-5 accuracy:  {:.1}% ({}/{})",
        pct(metrics.top5, metrics.total),
        metrics.top5,
        metrics.total
    );
    println!(
        "Not-found rate:  {:.1}% ({}/{})",
        pct(metrics.not_found, metrics.total),
        metrics.not_found,
        metrics.total
    );
    println!(
        "Rank-miss rate:  {:.1}% ({}/{})",
        pct(metrics.rank_miss, metrics.total),
        metrics.rank_miss,
        metrics.total
    );

    // Per-tag breakdown (only when tags are present).
    if !metrics.per_tag.is_empty() {
        println!();
        println!("--- Per-tag breakdown ---");
        let mut tags: Vec<&String> = metrics.per_tag.keys().collect();
        tags.sort();
        for tag in tags {
            let (top1, total) = metrics.per_tag[tag];
            println!(
                "  [{tag}]  Top-1: {:.1}% ({top1}/{total})",
                pct(top1, total)
            );
        }
    }

    // Failure listing.
    let failures: Vec<&EvalCaseResult> = results.iter().filter(|r| !r.exact_match).collect();
    println!();
    println!("--- Failures ({}) ---", failures.len());

    for r in &failures {
        println!("  input:    {}", r.input);
        println!("  expected: {}", r.expected);
        println!("  actual:   {}", r.actual);
        if let Some(cat) = &r.error_category {
            println!("  category: {cat}");
        }

        if verbose {
            // Print per-segment detail for each failing case.
            for seg in &r.segments {
                let rank_str = seg
                    .rank
                    .map(|r| format!("rank={r}"))
                    .unwrap_or_else(|| "rank=?".to_string());
                let exp_str = seg
                    .expected_surface
                    .as_deref()
                    .unwrap_or("?");
                println!(
                    "    {}→{} (expected={exp_str}, {rank_str}, status={}) [{}]",
                    seg.reading,
                    seg.selected,
                    seg.status,
                    seg.candidates_top5.join(", ")
                );
            }
        }
        println!();
    }
}

// ---------------------------------------------------------------------------
// JSONL report writer
// ---------------------------------------------------------------------------

fn write_jsonl_report(
    report_path: &str,
    results: &[EvalCaseResult],
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write as _;

    let file = std::fs::File::create(report_path)
        .map_err(|e| format!("failed to create report file {report_path}: {e}"))?;
    let mut writer = std::io::BufWriter::new(file);

    for result in results {
        let line = serde_json::to_string(result)
            .map_err(|e| format!("failed to serialize result: {e}"))?;
        writeln!(writer, "{line}")?;
    }

    writer.flush()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Main eval runner
// ---------------------------------------------------------------------------

/// Run evaluation on a JSONL file and report metrics.
fn run_eval(
    eval_path: &str,
    verbose: bool,
    report_path: Option<&str>,
    tag_filter: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dict_path = find_dict_path().ok_or(
        "cannot find data/dict/system.dict — \
         run from the project root or install the dictionary alongside the binary",
    )?;

    let dict = SimpleDictionary::load_from_file(&dict_path)
        .map_err(|e| format!("failed to load dictionary: {e}"))?;

    let text = std::fs::read_to_string(eval_path)
        .map_err(|e| format!("failed to read {eval_path}: {e}"))?;

    let mut results: Vec<EvalCaseResult> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let case: EvalCase = serde_json::from_str(line)
            .map_err(|e| format!("failed to parse JSONL line: {e}\n  line: {line}"))?;

        // Apply tag filter if specified.
        if let Some(filter_tag) = tag_filter {
            if !case.tags.iter().any(|t| t == filter_tag) {
                continue;
            }
        }

        let result = evaluate_case(&case, &dict);
        results.push(result);
    }

    let metrics = compute_metrics(&results);
    print_summary(eval_path, &metrics, &results, verbose);

    // Write JSONL report if requested.
    if let Some(path) = report_path {
        write_jsonl_report(path, &results)?;
        eprintln!("report written to: {path}");
    }

    Ok(())
}
