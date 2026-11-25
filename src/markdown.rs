//! Markdown markers are formatting, not words: `**bold**` should be read as
//! "bold", not "asterisk asterisk bold". The editor keeps showing them; only
//! the voice skips them.

use fancy_regex::{Captures, Regex};
use std::sync::OnceLock;

/// Blank out markdown markers with spaces, byte for byte, so every offset
/// into the result is an offset into `text` — word highlights still land on
/// the editor's own characters.
pub fn mask(text: &str) -> String {
    let mut out = text.to_string();
    for (re, keep) in rules() {
        out = re.replace_all(&out, |c: &Captures| blank_except(c, *keep)).into_owned();
    }
    out
}

/// Each rule blanks its whole match except capture group `keep`, if any.
/// Order matters: block markers first, then inline ones.
fn rules() -> &'static [(Regex, Option<usize>)] {
    static RULES: OnceLock<Vec<(Regex, Option<usize>)>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            // code fence lines, language tag included
            (r"(?m)^[ \t]*(?:```|~~~)[^\n]*$", None),
            // horizontal rules: --- *** ___
            (r"(?m)^[ \t]*([-*_])(?:[ \t]*\1){2,}[ \t]*$", None),
            // blockquotes, headings, bullets (numbered lists are read as-is)
            (r"(?m)^[ \t]*(?:>[ \t]?)*(?:#{1,6}[ \t]+|[-*+][ \t]+)?", None),
            // inline code
            (r"`+", None),
            // [text](url) and ![alt](url): keep the text
            (r"!?\[([^\]\n]*)\]\([^)\n]*\)", Some(1)),
            // **bold**, __bold__, ~~strike~~
            (r"\*{2,}|_{2,}|~{2,}", None),
            // *em*, _em_ — not 2 * 3, 5*6 or snake_case
            (r"(?<![\w*])([*_])(?=[^\s*_])(.+?)(?<=[^\s*_])\1(?![\w*])", Some(2)),
        ]
        .into_iter()
        .map(|(p, k)| (Regex::new(p).unwrap(), k))
        .collect()
    })
}

fn blank_except(c: &Captures, keep: Option<usize>) -> String {
    let whole = c.get(0).unwrap();
    match keep.and_then(|k| c.get(k)) {
        Some(k) => format!(
            "{}{}{}",
            " ".repeat(k.start() - whole.start()),
            k.as_str(),
            " ".repeat(whole.end() - k.end())
        ),
        None => " ".repeat(whole.as_str().len()),
    }
}

#[cfg(test)]
mod tests {
    use super::mask;

    fn sp(n: usize) -> String {
        " ".repeat(n)
    }

    #[test]
    fn emphasis_markers_are_blanked_in_place() {
        assert_eq!(
            mask("**bold** and *it* and __u__ and _i_ and ~~s~~"),
            "  bold   and  it  and   u   and  i  and   s  "
        );
    }

    #[test]
    fn line_start_markers_are_blanked() {
        assert_eq!(
            mask("# Title\n- one\n* two\n> quote\n---\n1. keep"),
            "  Title\n  one\n  two\n  quote\n   \n1. keep"
        );
    }

    #[test]
    fn links_and_images_keep_only_their_text() {
        // "[" blanked, "the docs" kept, "](https://x.y)" (14 bytes) blanked
        let want = format!("see {}the docs{} and {}pic{}", sp(1), sp(14), sp(2), sp(8));
        assert_eq!(mask("see [the docs](https://x.y) and ![pic](a.png)"), want);
    }

    #[test]
    fn inline_code_and_fences_are_blanked() {
        assert_eq!(mask("run `cargo test` now"), "run  cargo test  now");
        assert_eq!(mask("```rust\nlet x;\n```"), "       \nlet x;\n   ");
    }

    #[test]
    fn ordinary_punctuation_is_left_alone() {
        let plain = "snake_case_name, 2 * 3 * 4, 5*6, #1 fan, well-known - fine.";
        assert_eq!(mask(plain), plain);
    }

    #[test]
    fn byte_length_is_preserved_with_multibyte_text() {
        let s = "**café** — [naïve](u) `ü` ✓";
        let m = mask(s);
        assert_eq!(m.len(), s.len());
        assert!(m.contains("café") && m.contains("naïve") && !m.contains('*'));
    }
}
