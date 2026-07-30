//! PDF-paste cleanup, ported verbatim from the web app's tidyText/looksMessy.

use fancy_regex::Regex;
use std::sync::OnceLock;

fn rules() -> &'static [(Regex, &'static str)] {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            // hyphen at a wrapped line end: "en-\ngines"
            (r"([a-z])-[ \t]*\n[ \t]*([a-z])", "$1$2"),
            // mid-line split: "en- gines" (but keep "two- or three-")
            (r"([a-z])- (?!(?:or|and|nor|to|the)\b)([a-z])", "$1$2"),
            // citations: [10], [1, 2], [3-5]
            (r"\s*\[\d[\d,\s–-]*\]", ""),
            // ",," left behind by removed citations
            (r",\s*(?=[,.;:])", ""),
            // "IV. M ETHODOLOGY" heading artifacts
            (r"((?:^|\n|\. )[IVXLC]+\.\s+)([A-Z]) ([A-Z]{3,})", "$1$2$3"),
            // unwrap hard line breaks (keep paragraph breaks)
            (r"([^\n])\n(?!\n)", "$1 "),
            (r"[ \t]{2,}", " "),
        ]
        .into_iter()
        .map(|(p, r)| (Regex::new(p).unwrap(), r))
        .collect()
    })
}

/// The whole auto-tidy policy in one place: clean it only if it reads as PDF
/// debris and the cleanup actually changes something without emptying it.
pub fn tidy_if_messy(text: &str) -> Option<String> {
    if !looks_messy(text) {
        return None;
    }
    let cleaned = tidy(text);
    (cleaned != text && !cleaned.trim().is_empty()).then_some(cleaned)
}

pub fn tidy(text: &str) -> String {
    let mut out = text.to_string();
    for (re, rep) in rules() {
        out = re.replace_all(&out, *rep).into_owned();
    }
    out.trim().to_string()
}

pub fn looks_messy(text: &str) -> bool {
    static PATTERNS: OnceLock<Vec<(Regex, usize)>> = OnceLock::new();
    let pats = PATTERNS.get_or_init(|| {
        vec![
            (Regex::new(r"[a-z]- [a-z]").unwrap(), 2),
            (Regex::new(r"\[\d[\d,\s–-]*\]").unwrap(), 3),
            (Regex::new(r"[a-z,;] ?\n(?!\n)[a-z]").unwrap(), 5),
        ]
    });
    pats.iter()
        .any(|(re, min)| re.find_iter(text).filter(|m| m.is_ok()).count() >= *min)
}

#[cfg(test)]
mod tests {
    #[test]
    fn messy_pdf_paste_is_tidied() {
        // auto-tidy on paste cleans exactly this shape:
        // ≥5 mid-sentence single-newline breaks trip looks_messy
        let pasted = "Model IR channels and proc state as first-class graph\n\
                      nodes connected by synthetic binding edges, so channel\n\
                      and state edits flow through the existing node and edge\n\
                      differencing and patching machinery and are enacted\n\
                      through native channel and state APIs, removing\n\
                      the manual fixup step.";
        assert!(crate::tidy::looks_messy(pasted), "PDF-style line breaks read as messy");
        let cleaned = crate::tidy::tidy(pasted);
        assert!(!cleaned.contains("graph\nnodes"), "mid-sentence breaks should be joined");
        assert!(cleaned.contains("graph nodes"), "joined with a space");
    }
}
