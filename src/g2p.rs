//! Text → IPA phonemes → Kokoro token ids.
//!
//! Faithful port of the pipeline Python kokoro-onnx uses (phonemizer +
//! espeak-ng + vocab filter). Several oddities are replicated on purpose —
//! token parity with the Python oracle is the acceptance test.

use anyhow::{bail, Context, Result};
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::PathBuf;

pub const MAX_PHONEME_LENGTH: usize = 510;
const MARKS: &str = ";:,.!?¡¿—…\"«»“”(){}[]";

type InitFn = unsafe extern "C" fn(c_int, c_int, *const c_char, c_int) -> c_int;
type SetVoiceFn = unsafe extern "C" fn(*const c_char) -> c_int;
type TextToPhonemesFn =
    unsafe extern "C" fn(*mut *const c_void, c_int, c_int) -> *const c_char;

pub struct Espeak {
    lib: &'static libloading::Library,
}

fn candidates(file: &str) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("vendor").join(file)); // installed / AppDir usr/bin/vendor
        }
    }
    // AppImage: assets bundled under $APPDIR in the usual FHS spots
    if let Ok(appdir) = std::env::var("APPDIR") {
        let base = PathBuf::from(appdir);
        v.push(base.join("usr/bin/vendor").join(file));
        v.push(base.join("usr/lib").join(file));
        v.push(base.join("usr/share").join(file));
    }
    v.push(PathBuf::from("vendor").join(file));
    v.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor").join(file));
    v
}

pub fn find_espeak_lib() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MTTS_ESPEAK_LIB") {
        return Some(PathBuf::from(p));
    }
    for name in ["libespeak-ng.so", "libespeak-ng.so.1"] {
        for c in candidates(name) {
            if c.is_file() {
                return Some(c);
            }
        }
    }
    for sys in ["/usr/lib64/libespeak-ng.so.1", "/usr/lib/x86_64-linux-gnu/libespeak-ng.so.1"] {
        let p = PathBuf::from(sys);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

pub fn find_espeak_data() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MTTS_ESPEAK_DATA") {
        return Some(PathBuf::from(p));
    }
    for c in candidates("espeak-ng-data") {
        if c.is_dir() {
            return Some(c);
        }
    }
    let sys = PathBuf::from("/usr/share/espeak-ng-data");
    sys.is_dir().then_some(sys)
}

impl Espeak {
    pub fn new() -> Result<Self> {
        let lib_path = find_espeak_lib().context("no libespeak-ng found (vendor/ or system)")?;
        let data_path = find_espeak_data().context("no espeak-ng-data found (vendor/ or system)")?;
        let data_str = data_path.to_str().context("non-utf8 espeak data path")?;
        // espeak-ng has a small fixed internal buffer for this path; long
        // install paths silently fall back to a baked-in build path.
        if data_str.len() > 150 {
            bail!("espeak data path too long ({} chars): {data_str}", data_str.len());
        }
        let lib = Box::leak(Box::new(unsafe { libloading::Library::new(&lib_path) }
            .with_context(|| format!("dlopen {}", lib_path.display()))?));
        unsafe {
            let init: libloading::Symbol<InitFn> = lib.get(b"espeak_Initialize")?;
            let data = CString::new(data_str)?;
            // 0x02 = AUDIO_OUTPUT_SYNCHRONOUS, options 0 — as phonemizer does.
            if init(0x02, 0, data.as_ptr(), 0) <= 0 {
                bail!("espeak_Initialize failed (data: {data_str})");
            }
            let setv: libloading::Symbol<SetVoiceFn> = lib.get(b"espeak_SetVoiceByName")?;
            let v = CString::new("en-us")?;
            if setv(v.as_ptr()) != 0 {
                bail!("espeak_SetVoiceByName(en-us) failed");
            }
        }
        Ok(Self { lib })
    }

    /// espeak_TextToPhonemes clause loop: UTF-8 in, IPA out with '_' between
    /// phonemes, clauses joined by ' '. (phonemizer wrapper parity)
    fn text_to_phonemes(&self, text: &str) -> Result<String> {
        let mode = ((b'_' as c_int) << 8) | 0x02;
        let cstr = CString::new(text.replace('\0', ""))?;
        let mut ptr: *const c_void = cstr.as_ptr() as *const c_void;
        let mut parts: Vec<String> = Vec::new();
        unsafe {
            let f: libloading::Symbol<TextToPhonemesFn> =
                self.lib.get(b"espeak_TextToPhonemes")?;
            while !ptr.is_null() {
                let r = f(&mut ptr as *mut *const c_void, 1, mode);
                if !r.is_null() {
                    let s = CStr::from_ptr(r).to_string_lossy().into_owned();
                    if !s.is_empty() {
                        parts.push(s);
                    }
                }
            }
        }
        Ok(parts.join(" "))
    }
}

/// phonemizer EspeakBackend._postprocess_line with its exact defaults:
/// strip=False, word sep ' ', phone sep '' (drops the '_'), stress kept.
fn postprocess_line(line: &str) -> String {
    let line = line.trim().replace('\n', " ").replace("  ", " ");
    // espeak-ng#694 workaround, same order as phonemizer
    let mut collapsed = String::with_capacity(line.len());
    let mut prev_us = false;
    for c in line.chars() {
        if c == '_' {
            if !prev_us {
                collapsed.push(c);
            }
            prev_us = true;
        } else {
            collapsed.push(c);
            prev_us = false;
        }
    }
    let line = collapsed.replace("_ ", " ");
    if line.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(line.len());
    for word in line.split(' ') {
        out.push_str(&word.trim().replace('_', ""));
        out.push(' ');
    }
    out // trailing ' ' kept (strip=False)
}

#[derive(Debug, Clone)]
struct Mark {
    text: String,
    position: char, // B, E, I, A
}

/// phonemizer Punctuation.preserve: split a line into punctuation-free chunks
/// plus a mark list. A "match" is a maximal run of whitespace/marks containing
/// at least one mark (regex `(\s*[marks]+\s*)+`).
fn preserve(line: &str) -> (Vec<String>, Vec<Mark>) {
    let chars: Vec<char> = line.chars().collect();
    let is_mark = |c: char| MARKS.contains(c);
    let mut matches: Vec<String> = Vec::new();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if is_mark(chars[i]) || chars[i].is_whitespace() {
            let st = i;
            let mut any_mark = false;
            while i < chars.len() && (is_mark(chars[i]) || chars[i].is_whitespace()) {
                any_mark |= is_mark(chars[i]);
                i += 1;
            }
            if any_mark {
                spans.push((st, i));
                matches.push(chars[st..i].iter().collect());
            }
        } else {
            i += 1;
        }
    }
    if matches.is_empty() {
        return (vec![line.to_string()], vec![]);
    }
    if matches.len() == 1 && matches[0].chars().count() == chars.len() {
        return (vec![], vec![Mark { text: line.to_string(), position: 'A' }]);
    }
    let mut marks = Vec::new();
    let n = matches.len();
    for (k, m) in matches.iter().enumerate() {
        let position = if k == 0 && spans[0].0 == 0 {
            'B'
        } else if k == n - 1 && spans[n - 1].1 == chars.len() {
            'E'
        } else {
            'I'
        };
        marks.push(Mark { text: m.clone(), position });
    }
    // progressive split at the first occurrence of each mark string
    let mut chunks = Vec::new();
    let mut rest = line.to_string();
    for m in &marks {
        match rest.split_once(&m.text) {
            Some((prefix, suffix)) => {
                chunks.push(prefix.to_string());
                rest = suffix.to_string();
            }
            None => chunks.push(String::new()),
        }
    }
    chunks.push(rest);
    (chunks.into_iter().filter(|c| !c.is_empty()).collect(), marks)
}

/// phonemizer Punctuation.restore, verbatim — including the `pos` quirk where
/// marks after the first E/A never match again on a single line.
fn restore(mut chunks: Vec<String>, mut marks: Vec<Mark>) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while !chunks.is_empty() || !marks.is_empty() {
        if marks.is_empty() {
            for mut line in chunks.drain(..) {
                if !line.ends_with(' ') {
                    line.push(' ');
                }
                out.push(line);
            }
        } else if chunks.is_empty() {
            let joined: String = marks.drain(..).map(|m| m.text).collect();
            out.push(joined);
        } else if pos == 0 {
            let mark = marks.remove(0);
            if chunks[0].ends_with(' ') {
                let l = chunks[0].len() - 1;
                chunks[0].truncate(l);
            }
            match mark.position {
                'B' => chunks[0] = format!("{}{}", mark.text, chunks[0]),
                'E' => {
                    let head = chunks.remove(0);
                    let tail = if mark.text.ends_with(' ') { "" } else { " " };
                    out.push(format!("{head}{}{tail}", mark.text));
                    pos += 1;
                }
                'A' => {
                    let tail = if mark.text.ends_with(' ') { "" } else { " " };
                    out.push(format!("{}{tail}", mark.text));
                    pos += 1;
                }
                _ => {
                    if chunks.len() == 1 {
                        chunks[0] = format!("{}{}", chunks[0], mark.text);
                    } else {
                        let first = chunks.remove(0);
                        chunks[0] = format!("{first}{}{}", mark.text, chunks[0]);
                    }
                }
            }
        } else {
            out.push(chunks.remove(0));
        }
    }
    out
}

pub struct G2p {
    espeak: Espeak,
    pub vocab: HashMap<char, i64>,
    /// lowercased term -> respelling, applied before espeak sees the text
    lexicon: HashMap<String, String>,
}

/// User pronunciation overrides: `term<TAB>respelling`, one per line, `#`
/// comments. Respell in plain English ("kubectl<TAB>koob cuttle") — espeak
/// pronounces the replacement, so no phonetic alphabet is needed.
pub fn lexicon_path() -> PathBuf {
    crate::synth::config_dir().join("lexicon.tsv")
}

fn load_lexicon() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(text) = std::fs::read_to_string(lexicon_path()) else { return map };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // tab-separated; fall back to the first run of whitespace
        let Some((term, say)) = line.split_once('\t').or_else(|| line.split_once("  ")) else {
            continue;
        };
        let (term, say) = (term.trim(), say.trim());
        if !term.is_empty() && !say.is_empty() {
            map.insert(term.to_lowercase(), say.to_string());
        }
    }
    map
}

/// The replacement for one word, or None to keep it. Tries the word as-is
/// first so the common all-lowercase case never allocates.
fn lookup<'a>(word: &str, lexicon: &'a HashMap<String, String>) -> Option<&'a str> {
    if let Some(say) = lexicon.get(word) {
        return Some(say);
    }
    word.chars()
        .any(char::is_uppercase)
        .then(|| lexicon.get(&word.to_lowercase()))
        .flatten()
        .map(String::as_str)
}

/// Replace whole words the lexicon knows about. Word boundaries are the same
/// alphanumeric/apostrophe runs espeak would tokenize, so surrounding
/// punctuation is untouched and the caller's text is left alone. Borrows
/// straight through when nothing matches, which is almost every sentence.
fn respell<'a>(text: &'a str, lexicon: &HashMap<String, String>) -> Cow<'a, str> {
    let mut out: Option<String> = None;
    let mut copied = 0; // bytes of `text` already flushed into `out`
    let mut start = None; // start of the word run being scanned
    for (i, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        if c.is_alphanumeric() || c == '\'' {
            start.get_or_insert(i);
            continue;
        }
        if let Some(s) = start.take() {
            if let Some(say) = lookup(&text[s..i], lexicon) {
                let out = out.get_or_insert_with(|| String::with_capacity(text.len()));
                out.push_str(&text[copied..s]);
                out.push_str(say);
                copied = i;
            }
        }
    }
    match out {
        Some(mut out) => {
            out.push_str(&text[copied..]);
            Cow::Owned(out)
        }
        None => Cow::Borrowed(text),
    }
}


impl G2p {
    pub fn new() -> Result<Self> {
        let cfg: serde_json::Value = serde_json::from_str(include_str!("kokoro-config.json"))?;
        let mut vocab = HashMap::new();
        for (k, v) in cfg["vocab"].as_object().context("config vocab")? {
            let mut it = k.chars();
            let (Some(c), None) = (it.next(), it.next()) else {
                bail!("multi-char vocab key {k:?}");
            };
            vocab.insert(c, v.as_i64().context("vocab id")?);
        }
        Ok(Self { espeak: Espeak::new()?, vocab, lexicon: load_lexicon() })
    }

    /// Text as espeak should see it: lexicon terms respelled, borrowed
    /// unchanged when there is no lexicon or nothing matches.
    fn respelled<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if self.lexicon.is_empty() {
            Cow::Borrowed(text)
        } else {
            respell(text, &self.lexicon)
        }
    }

    /// How many pronunciation overrides are loaded (0 = no lexicon file).
    pub fn lexicon_len(&self) -> usize {
        self.lexicon.len()
    }

    /// Full pipeline: text → filtered IPA phoneme string (kokoro tokenizer
    /// parity: phonemize, drop non-vocab chars, strip).
    pub fn phonemize(&self, text: &str) -> Result<String> {
        let line = text.trim().trim_matches('\n');
        if line.trim().is_empty() {
            return Ok(String::new());
        }
        // respell before espeak only — the caller's text keeps its own words, so
        // word timings and the reader's highlight boxes are unaffected
        let line = self.respelled(line);
        let (chunks, marks) = preserve(&line);
        let mut phon = Vec::with_capacity(chunks.len());
        for ch in &chunks {
            phon.push(postprocess_line(&self.espeak.text_to_phonemes(ch)?));
        }
        let joined = restore(phon, marks).join("\n");
        let filtered: String = joined.chars().filter(|c| self.vocab.contains_key(c)).collect();
        Ok(filtered.trim().to_string())
    }

    /// Phonemize a single word in isolation (used only for merge alignment,
    /// never for model input — isolated pronunciation differs slightly).
    /// Respells too, or its phonemes stop matching what phonemize() produced.
    pub fn phonemize_solo(&self, word: &str) -> Result<String> {
        let word = self.respelled(word);
        let raw = self.espeak.text_to_phonemes(&word)?;
        let post = postprocess_line(&raw);
        let filtered: String = post.chars().filter(|c| self.vocab.contains_key(c)).collect();
        Ok(filtered.trim().to_string())
    }

    pub fn tokenize(&self, phonemes: &str) -> Vec<i64> {
        phonemes.chars().filter_map(|c| self.vocab.get(&c).copied()).collect()
    }
}

#[cfg(test)]
mod lexicon_tests {
    use super::*;

    #[test]
    fn respell_matches_whole_words_case_insensitively() {
        let lex: HashMap<String, String> = [
            ("kubectl", "koob cuttle"),
            ("aws", "ay double you ess"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        // punctuation around a match is preserved, and case doesn't matter
        assert_eq!(respell("Run kubectl, then AWS.", &lex), "Run koob cuttle, then ay double you ess.");
        // whole words only — no substring hits
        assert_eq!(respell("kubectlx and xkubectl", &lex), "kubectlx and xkubectl");
        // untouched text comes back byte-identical
        assert_eq!(respell("nothing to see here", &lex), "nothing to see here");
        assert_eq!(respell("naïve café — dash", &lex), "naïve café — dash");
    }
}
