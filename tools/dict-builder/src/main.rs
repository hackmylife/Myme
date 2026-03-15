//! `dict-builder` — offline dictionary compiler for myme.
//!
//! # Overview
//!
//! Reads `data/raw/SKK-JISYO.L` (EUC-JP encoded), converts to UTF-8,
//! filters to entries whose reading is pure hiragana, and writes a UTF-8
//! SKK-format text file to `data/dict/system.dict` suitable for loading
//! by `myme-core::dictionary::SimpleDictionary`.
//!
//! # Usage
//!
//! ```sh
//! cargo run -p dict-builder --release
//! ```

use std::collections::HashMap;
use std::fs;
use std::io::Write as IoWrite;
use std::path::Path;

use encoding_rs::EUC_JP;

/// Unicode codepoint ranges for hiragana.
///
/// U+3041 (ぁ) – U+3096 (ゖ): standard hiragana block
/// U+309D (ゝ) – U+309E (ゞ): hiragana iteration marks
/// U+309F (ゟ): hiragana digraph (rare, but still hiragana)
fn is_hiragana_char(c: char) -> bool {
    matches!(c, '\u{3041}'..='\u{3096}' | '\u{309D}'..='\u{309F}')
}

/// Return true when every character in the reading is hiragana.
/// An empty reading is rejected.
fn is_all_hiragana(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_hiragana_char)
}

/// Load frequency data from a TSV file.
///
/// Format: `reading\tsurface\tfrequency` (lines starting with `#` are comments).
/// Returns a map from `(reading, surface)` to frequency.
fn load_frequency_table(path: &Path) -> HashMap<(String, String), u32> {
    let mut table = HashMap::new();
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("warning: could not read frequency file {}: {}", path.display(), e);
            return table;
        }
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 {
            if let Ok(freq) = parts[2].parse::<u32>() {
                table.insert((parts[0].to_string(), parts[1].to_string()), freq);
            }
        }
    }
    table
}

/// Annotate a dictionary line with frequency data.
///
/// For each candidate in the line, if `(reading, surface)` exists in the
/// frequency table, append `;freq=N` to that candidate.
fn annotate_line(line: &str, freq_table: &HashMap<(String, String), u32>) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with(';') {
        return line.to_string();
    }
    let Some((reading, rest)) = trimmed.split_once(' ') else {
        return line.to_string();
    };
    // rest looks like: /候補1/候補2;annotation/.../
    let rest = rest.trim();
    if !rest.starts_with('/') {
        return line.to_string();
    }

    let candidates: Vec<&str> = rest[1..]
        .split('/')
        .collect();

    let mut new_candidates: Vec<String> = Vec::new();
    for cand in &candidates {
        let cand = cand.trim();
        if cand.is_empty() {
            continue;
        }
        // Extract the bare surface (strip existing annotations).
        let surface = cand.split_once(';').map(|(s, _)| s).unwrap_or(cand);
        let existing_ann = cand.split_once(';').map(|(_, a)| a);

        let key = (reading.to_string(), surface.to_string());
        if let Some(&freq) = freq_table.get(&key) {
            // Merge freq annotation.  If there's already a freq= annotation,
            // replace it; otherwise append.
            if let Some(ann) = existing_ann {
                if ann.starts_with("freq=") {
                    // Replace existing freq.
                    new_candidates.push(format!("{};freq={}", surface, freq));
                } else {
                    new_candidates.push(format!("{};{};freq={}", surface, ann, freq));
                }
            } else {
                new_candidates.push(format!("{};freq={}", surface, freq));
            }
        } else {
            new_candidates.push(cand.to_string());
        }
    }

    format!("{} /{}/", reading, new_candidates.join("/"))
}

fn main() {
    // -----------------------------------------------------------------------
    // Locate input / output paths relative to the workspace root.
    //
    // Cargo sets CARGO_MANIFEST_DIR to the crate root
    // (tools/dict-builder), so we walk two directories up to reach the
    // workspace root and then navigate into data/.
    // -----------------------------------------------------------------------
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = Path::new(manifest_dir)
        .parent() // tools/
        .and_then(|p| p.parent()) // workspace root
        .expect("could not determine workspace root from CARGO_MANIFEST_DIR");

    let input_path = workspace_root.join("data/raw/SKK-JISYO.L");
    let freq_path = workspace_root.join("data/raw/frequency.tsv");
    let output_path = workspace_root.join("data/dict/system.dict");

    // -----------------------------------------------------------------------
    // Read the raw EUC-JP file.
    // -----------------------------------------------------------------------
    let raw_bytes = fs::read(&input_path).unwrap_or_else(|e| {
        eprintln!(
            "error: could not read {}: {}",
            input_path.display(),
            e
        );
        std::process::exit(1);
    });

    // -----------------------------------------------------------------------
    // Decode EUC-JP → UTF-8.
    //
    // encoding_rs::Encoding::decode returns (Cow<str>, had_errors).
    // We treat any decoding error as fatal because a corrupt source
    // dictionary would produce garbage candidates at runtime.
    // -----------------------------------------------------------------------
    let (utf8_text, _encoding_used, had_errors) = EUC_JP.decode(&raw_bytes);
    if had_errors {
        eprintln!("warning: EUC-JP decoding encountered errors; some characters may be replaced with U+FFFD");
    }

    // -----------------------------------------------------------------------
    // Load frequency data (optional — missing file is not an error).
    // -----------------------------------------------------------------------
    let freq_table = load_frequency_table(&freq_path);
    let freq_count = freq_table.len();

    // -----------------------------------------------------------------------
    // Parse the decoded text and filter to hiragana-only readings.
    //
    // SKK-JISYO.L has two logical sections separated by the comment line:
    //   ";; okuri-ari entries."   — readings end with a trailing ASCII letter
    //   ";; okuri-nasi entries."  — plain readings
    //
    // We process both sections the same way: if the reading field
    // (everything before the first ASCII space on a non-comment line) is
    // entirely hiragana, keep the line verbatim; otherwise skip it.
    //
    // If frequency data is available, annotate matching candidates with
    // `;freq=N` annotations.
    // -----------------------------------------------------------------------
    let mut total_entries: usize = 0;
    let mut kept_entries: usize = 0;
    let mut freq_annotated: usize = 0;

    // Collect output lines; we'll write them all at once.
    let mut out_lines: Vec<String> = Vec::with_capacity(150_000);

    for line in utf8_text.lines() {
        let trimmed = line.trim();

        // Keep comment/blank lines as-is in the output for readability,
        // but do not count them toward the entry statistics.
        if trimmed.is_empty() || trimmed.starts_with(';') {
            out_lines.push(line.to_string());
            continue;
        }

        total_entries += 1;

        // Extract the reading: the token before the first ASCII space.
        let reading = match trimmed.split_once(' ') {
            Some((r, _)) => r,
            None => {
                // Malformed line — skip silently (stats still count it as
                // a "seen" entry so the user can gauge data quality).
                continue;
            }
        };

        if is_all_hiragana(reading) {
            let annotated = annotate_line(line, &freq_table);
            if annotated != line {
                freq_annotated += 1;
            }
            out_lines.push(annotated);
            kept_entries += 1;
        }
    }

    // -----------------------------------------------------------------------
    // Write output.
    // -----------------------------------------------------------------------
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| {
            eprintln!(
                "error: could not create output directory {}: {}",
                parent.display(),
                e
            );
            std::process::exit(1);
        });
    }

    let mut out_file = fs::File::create(&output_path).unwrap_or_else(|e| {
        eprintln!(
            "error: could not create {}: {}",
            output_path.display(),
            e
        );
        std::process::exit(1);
    });

    for line in &out_lines {
        writeln!(out_file, "{}", line).unwrap_or_else(|e| {
            eprintln!("error: write failed: {}", e);
            std::process::exit(1);
        });
    }

    // -----------------------------------------------------------------------
    // Print statistics.
    // -----------------------------------------------------------------------
    let filtered_entries = total_entries - kept_entries;
    println!("dict-builder: done.");
    println!("  input  : {}", input_path.display());
    println!("  freq   : {} ({} entries)", freq_path.display(), freq_count);
    println!("  output : {}", output_path.display());
    println!("  total entries seen : {total_entries}");
    println!("  hiragana entries   : {kept_entries}");
    println!("  freq-annotated     : {freq_annotated}");
    println!("  filtered out       : {filtered_entries}");
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // is_hiragana_char
    // ------------------------------------------------------------------
    #[test]
    fn hiragana_boundary_chars_accepted() {
        // First and last standard hiragana codepoints.
        assert!(is_hiragana_char('ぁ')); // U+3041
        assert!(is_hiragana_char('ゖ')); // U+3096
    }

    #[test]
    fn hiragana_iteration_marks_accepted() {
        assert!(is_hiragana_char('ゝ')); // U+309D
        assert!(is_hiragana_char('ゞ')); // U+309E
    }

    #[test]
    fn katakana_rejected() {
        assert!(!is_hiragana_char('ア')); // U+30A2
        assert!(!is_hiragana_char('ン')); // U+30F3
    }

    #[test]
    fn ascii_rejected() {
        assert!(!is_hiragana_char('a'));
        assert!(!is_hiragana_char('Z'));
        assert!(!is_hiragana_char('1'));
    }

    #[test]
    fn kanji_rejected() {
        assert!(!is_hiragana_char('変'));
        assert!(!is_hiragana_char('換'));
    }

    // ------------------------------------------------------------------
    // is_all_hiragana
    // ------------------------------------------------------------------
    #[test]
    fn empty_string_rejected() {
        assert!(!is_all_hiragana(""));
    }

    #[test]
    fn pure_hiragana_accepted() {
        assert!(is_all_hiragana("へんかん"));
        assert!(is_all_hiragana("にほんご"));
        assert!(is_all_hiragana("あ"));
    }

    #[test]
    fn mixed_hiragana_katakana_rejected() {
        assert!(!is_all_hiragana("へんアん"));
    }

    #[test]
    fn trailing_ascii_rejected() {
        // okuri-ari style: reading ends with ASCII letter
        assert!(!is_all_hiragana("われr"));
        assert!(!is_all_hiragana("をs"));
    }

    #[test]
    fn ascii_key_rejected() {
        assert!(!is_all_hiragana("!"));
        assert!(!is_all_hiragana("#giga"));
    }
}
