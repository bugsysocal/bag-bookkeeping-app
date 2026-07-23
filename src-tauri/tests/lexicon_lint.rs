//! Spec 07 §4.1 Decision #1: forbidden double-entry words never appear in
//! Owner Mode UI strings — enforced mechanically, not by review discipline
//! alone. This is the project's "build-time lint": `ui/index.html` has no
//! separate build step, so the check runs the same way every other check in
//! this project does, via `cargo test`.
//!
//! Scoping: `ui/index.html` is one file with no owner/advisor code split, so
//! exemptions are explicit, not inferred — a function legitimately using
//! these words (Advisor Mode screens, formal statements, or non-bookkeeping
//! text like the EULA) is marked with a `// LEXICON-EXEMPT` comment on the
//! line directly above its `function`/`async function` declaration. Anything
//! not marked is Owner Mode and must be clean. Matching is whole-word,
//! splitting on anything that isn't a "word" character (alphanumeric or
//! underscore) — underscore stays part of the word so this doesn't flag
//! `debit`/`credit`/`journal`/`ledger` inside snake_case data field names
//! like `debit_col`, `journal_line_id`, `ledger_at_date_kobo` (real DTO/JS
//! property names, never shown to a user), or `ledger` inside `LedgerOne`,
//! or `contra` inside `contract`.

use std::fs;

const FORBIDDEN_WORDS: &[&str] =
    &["debit", "credit", "journal", "ledger", "posting", "accrual", "liability", "equity", "contra"];

fn tokens(line: &str) -> Vec<String> {
    line.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Lines belonging to a `// LEXICON-EXEMPT`-marked function: from the marker
/// line through the matching close-brace of the function it annotates.
fn exempt_lines(lines: &[&str]) -> Vec<bool> {
    let mut exempt = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("// LEXICON-EXEMPT") {
            let mut depth = 0i32;
            let mut started = false;
            let mut j = i;
            while j < lines.len() {
                for ch in lines[j].chars() {
                    match ch {
                        '{' => { depth += 1; started = true; }
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                exempt[j] = true;
                if started && depth <= 0 {
                    break;
                }
                j += 1;
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    exempt
}

#[test]
fn owner_mode_strings_avoid_the_forbidden_lexicon() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../ui/index.html");
    let text = fs::read_to_string(path).expect("read ui/index.html");
    let lines: Vec<&str> = text.lines().collect();
    let exempt = exempt_lines(&lines);

    let mut violations = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if exempt[idx] {
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
            continue; // developer comments aren't user-facing strings
        }
        let words = tokens(line);
        // "trial balance" is checked as a phrase; every other entry is a single word.
        let has_trial_balance = words.windows(2).any(|w| w[0] == "trial" && w[1] == "balance");
        if has_trial_balance {
            violations.push(format!("line {}: contains \"trial balance\" — {}", idx + 1, line.trim()));
        }
        for word in FORBIDDEN_WORDS {
            if words.iter().any(|w| w == word) {
                violations.push(format!("line {}: contains \"{word}\" — {}", idx + 1, line.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "Forbidden double-entry words found in Owner Mode UI strings (Spec 07 §4.1). \
         If this is genuinely Advisor Mode or formal-statement content, mark the containing \
         function with a `// LEXICON-EXEMPT` comment directly above its declaration:\n{}",
        violations.join("\n")
    );
}
