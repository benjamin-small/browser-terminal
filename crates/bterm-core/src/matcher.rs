//! Pattern matching for `grep`, injected by the host.
//!
//! The core stays wasm-free, so it can't reach a regex engine directly — and
//! we deliberately don't compile one in (the `regex` crate would dwarf the
//! rest of the binary). Instead the host supplies the matcher: the browser
//! injects one backed by JavaScript's native `RegExp` — full regex for zero
//! added bytes, since the engine is already there — while native hosts (the
//! CLI, tests) fall back to substring matching.
//!
//! Consequence worth knowing: `grep 'a|b'` is alternation in the browser and
//! a literal `a|b` in the CLI. The CLI is a dev harness, so patterns that
//! work there always work in the browser, never the reverse.

/// A compiled pattern. Not `Send`/`Sync` by design — the browser
/// implementation wraps a `js_sys::RegExp`, and the engine is single-threaded.
pub trait Pattern {
    fn is_match(&self, text: &str) -> bool;
}

/// Compiles patterns for `grep`.
pub trait PatternMatcher {
    /// Compile a pattern, or return a human-readable syntax error.
    fn compile(&self, pattern: &str, case_insensitive: bool) -> Result<Box<dyn Pattern>, String>;

    /// Shown in help and error messages: `regex` or `substring`.
    fn dialect(&self) -> &'static str;
}

/// Default matcher: plain substring containment.
pub struct SubstringMatcher;

struct SubstringPattern {
    /// Pre-lowercased when matching case-insensitively.
    needle: String,
    case_insensitive: bool,
}

impl Pattern for SubstringPattern {
    fn is_match(&self, text: &str) -> bool {
        if self.case_insensitive {
            text.to_lowercase().contains(&self.needle)
        } else {
            text.contains(&self.needle)
        }
    }
}

impl PatternMatcher for SubstringMatcher {
    fn compile(&self, pattern: &str, case_insensitive: bool) -> Result<Box<dyn Pattern>, String> {
        Ok(Box::new(SubstringPattern {
            needle: if case_insensitive {
                pattern.to_lowercase()
            } else {
                pattern.to_string()
            },
            case_insensitive,
        }))
    }

    fn dialect(&self) -> &'static str {
        "substring"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_matches_and_misses() {
        let p = SubstringMatcher.compile("rust", false).expect("compile");
        assert!(p.is_match("we love rust here"));
        assert!(!p.is_match("no match"));
        assert!(!p.is_match("RUST"), "case-sensitive by default");
    }

    #[test]
    fn substring_case_insensitive() {
        let p = SubstringMatcher.compile("RuSt", true).expect("compile");
        assert!(p.is_match("RUST LANG"));
        assert!(p.is_match("rust lang"));
    }

    #[test]
    fn empty_pattern_matches_everything() {
        let p = SubstringMatcher.compile("", false).expect("compile");
        assert!(p.is_match("anything"));
        assert!(p.is_match(""));
    }
}
