//! Kokoro synthesis via ONNX Runtime: audio + exact word timestamps.

use crate::g2p::{G2p, MAX_PHONEME_LENGTH};
use anyhow::{bail, Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const SAMPLE_RATE: u32 = 24_000;
/// Low-pass cutoff (Hz) for male/other voices: their speech ends ~6 kHz, so this
/// strips high-frequency model hiss with no loss. Female voices carry real
/// sibilance up to ~11 kHz (measured) and use LOWPASS_HZ_FEMALE so it isn't
/// dulled. Override either with MTTS_LOWPASS (0 = off).
const LOWPASS_HZ: f32 = 8000.0;
const LOWPASS_HZ_FEMALE: f32 = 11000.0;
/// Voice ids grouped for the picker; also what `--list-voices` prints.
pub const VOICE_GROUPS: &[(&str, &[&str])] = &[
    ("Custom", &["chad"]),
    ("US male", &["am_michael", "am_fenrir", "am_puck", "am_echo", "am_eric",
                  "am_liam", "am_onyx", "am_adam", "am_santa"]),
    ("UK male", &["bm_george", "bm_fable", "bm_lewis", "bm_daniel"]),
    ("US female", &["af_heart", "af_bella", "af_nicole", "af_aoede", "af_kore",
                    "af_sarah", "af_nova", "af_alloy", "af_sky", "af_jessica", "af_river"]),
    ("UK female", &["bf_emma", "bf_isabella", "bf_alice", "bf_lily"]),
];
pub const DEFAULT_VOICE: &str = "am_michael";

/// Every selectable voice id, in picker order.
pub fn voice_ids() -> impl Iterator<Item = &'static str> {
    VOICE_GROUPS.iter().flat_map(|(_, ids)| ids.iter().copied())
}

/// Nearest known voice to a typo, for "did you mean". None if nothing is close.
pub fn nearest_voice(name: &str) -> Option<&'static str> {
    voice_ids()
        .map(|id| (levenshtein(id, name), id))
        .filter(|&(d, _)| d <= 3)
        .min_by_key(|&(d, _)| d)
        .map(|(_, id)| id)
}

/// Trimmed sentence byte ranges over the raw text (long run-ons hard-split).
pub fn sentence_ranges(raw: &str) -> Vec<(usize, usize)> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut out = Vec::new();
    for (off, seg) in raw.split_sentence_bound_indices() {
        let t = seg.trim();
        if t.is_empty() {
            continue;
        }
        let lead = seg.len() - seg.trim_start().len();
        let (mut a, b) = (off + lead, off + lead + t.len());
        while b - a > 1200 {
            // break at a space within ~1000 bytes, snapped down to a char boundary
            let mut we = (a + 1000).min(b);
            while !raw.is_char_boundary(we) {
                we -= 1;
            }
            let cut = raw[a..we].rfind(' ').map(|c| a + c).unwrap_or(we);
            out.push((a, cut));
            let after = &raw[cut..];
            a = cut + (after.len() - after.trim_start().len());
        }
        out.push((a, b));
    }
    out
}

/// Weighted blends of preset voice vectors — more onyx = deeper.
pub const CUSTOM_VOICES: &[(&str, &[(&str, f32)])] =
    &[("chad", &[("am_puck", 0.4), ("am_onyx", 0.6)])];

// --- output low-pass -------------------------------------------------------

/// RBJ cookbook biquad, transposed direct form II.
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}
impl Biquad {
    fn lowpass(sr: f32, fc: f32, q: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * fc / sr;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Biquad {
            b0: (1.0 - cos) / 2.0 / a0,
            b1: (1.0 - cos) / a0,
            b2: (1.0 - cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }
    fn step(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// In-place 4th-order Butterworth low-pass (two cascaded biquads, 24 dB/oct).
fn low_pass(samples: &mut [f32], sr: f32, fc: f32) {
    if fc <= 0.0 || fc >= sr / 2.0 {
        return; // disabled / above Nyquist
    }
    let mut s1 = Biquad::lowpass(sr, fc, 0.541_196_1);
    let mut s2 = Biquad::lowpass(sr, fc, 1.306_563);
    for x in samples.iter_mut() {
        *x = s2.step(s1.step(*x));
    }
}

fn lowpass_hz(voice: &str) -> f32 {
    if let Some(v) = std::env::var("MTTS_LOWPASS").ok().and_then(|s| s.parse().ok()) {
        return v;
    }
    // Kokoro ids are <lang><gender>_name; second char 'f' = female
    if voice.as_bytes().get(1) == Some(&b'f') {
        LOWPASS_HZ_FEMALE
    } else {
        LOWPASS_HZ
    }
}

#[derive(Debug, Clone)]
pub struct WordTiming {
    pub start: f32,
    pub end: f32,
    pub char_start: usize, // byte offsets into the input text
    pub char_end: usize,
}

pub struct Synthesizer {
    sess: Session,
    /// voice id -> 510*256 row-major style table (row = token count)
    voices: HashMap<String, Vec<f32>>,
    pub g2p: G2p,
    solo_cache: std::cell::RefCell<HashMap<String, String>>,
}

pub fn models_dir() -> PathBuf {
    if let Ok(p) = std::env::var("MTTS_MODELS") {
        return PathBuf::from(p);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models");
    if manifest.is_dir() {
        return manifest;
    }
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("models")))
        .unwrap_or_else(|| PathBuf::from("models"))
}

/// `$XDG_<var>_HOME` with the conventional `$HOME/<fallback>` default.
pub fn xdg_dir(var: &str, home_fallback: &str) -> PathBuf {
    std::env::var(var).map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(home_fallback)
    })
}

/// Our directory under the XDG config home (state.json, lexicon.tsv).
pub fn config_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("minimal-tts")
}

/// The one WAV format everything writes: 16-bit mono at the model's rate.
pub fn wav_spec() -> hound::WavSpec {
    hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

/// Unity-gain sample conversion — clamp only, no boost, matching playback.
pub fn pcm16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// The bundled model (precision-agnostic name — swapping variants is a file
/// replacement), then the legacy name (any-model escape hatch for benching
/// via MTTS_MODELS). fp16 is NOT shippable: its graph NaNs out on blended
/// style vectors (the custom "chad" voice) — silent audio, verified 2026-07.
fn resolve_model(dir: &Path) -> Result<PathBuf> {
    for name in ["kokoro.onnx", "kokoro-ts.onnx"] {
        let p = dir.join(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    bail!("no model in {} — run scripts/get-models.sh (fetches fp32)", dir.display())
}

/// Minimal .npy parser (little-endian f32, C order) for entries of an .npz.
fn parse_npy_f32(buf: &[u8]) -> Result<Vec<f32>> {
    if buf.len() < 10 || &buf[..6] != b"\x93NUMPY" {
        bail!("not an npy");
    }
    let (major, header_len, data_off) = if buf[6] == 1 {
        (1u8, u16::from_le_bytes([buf[8], buf[9]]) as usize, 10usize)
    } else {
        (buf[6], u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]) as usize, 12usize)
    };
    let header = std::str::from_utf8(&buf[data_off..data_off + header_len])?;
    if !header.contains("<f4") || header.contains("'fortran_order': True") {
        bail!("unsupported npy layout (v{major}): {header}");
    }
    let data = &buf[data_off + header_len..];
    Ok(data
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

fn load_voices(path: &Path) -> Result<HashMap<String, Vec<f32>>> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut voices = HashMap::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().trim_end_matches(".npy").to_string();
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut buf)?;
        let data = parse_npy_f32(&buf)?;
        if data.len() % 256 != 0 {
            bail!("voice {name}: unexpected size {}", data.len());
        }
        voices.insert(name, data);
    }
    Ok(voices)
}

/// Batch the phoneme string under the model's context limit, breaking at word
/// boundaries (spaces). A batch never exceeds MAX_PHONEME_LENGTH, so a long
/// punctuation-free sentence is split across batches instead of truncated.
fn split_phonemes(phonemes: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for word in phonemes.split(' ') {
        if word.is_empty() {
            continue;
        }
        let wlen = word.chars().count();
        if cur_len > 0 && cur_len + 1 + wlen > MAX_PHONEME_LENGTH {
            parts.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        if cur_len > 0 {
            cur.push(' ');
            cur_len += 1;
        }
        cur.push_str(word);
        cur_len += wlen;
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

/// A phoneme-word: a maximal non-space run of the (filtered) phoneme string,
/// with absolute timing computed from the model's duration output.
#[derive(Debug, Clone)]
struct PhonemeWord {
    phonemes: String,
    start: f32,
    end: f32,
}

/// Verbatim port of kokoro KPipeline.join_timestamps: half-frame counters,
/// 600-sample frames at 24 kHz, divisor 80, space durations split around words.
fn walk_timestamps(filtered: &str, dur: &[f32], t_offset: f32) -> Vec<PhonemeWord> {
    let chars: Vec<char> = filtered.chars().collect();
    let mut words = Vec::new();
    if dur.len() < 3 {
        return words;
    }
    let mut left = 2.0 * (dur[0] - 3.0).max(0.0);
    let mut right = left;
    let mut i = 0usize;
    while i < chars.len() {
        while i < chars.len() && chars[i] == ' ' {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let st = i;
        while i < chars.len() && chars[i] != ' ' {
            i += 1;
        }
        // token k lives at dur[k + 1] (BOS pad at dur[0])
        let (di, dj) = (st + 1, i + 1);
        if dj >= dur.len() {
            break;
        }
        let start = left / 80.0;
        let token_dur: f32 = dur[di..dj].iter().sum();
        let space_dur = if i < chars.len() { dur[dj] } else { 0.0 };
        left = right + 2.0 * token_dur + space_dur;
        let end = left / 80.0;
        right = left + space_dur;
        words.push(PhonemeWord {
            phonemes: chars[st..i].iter().collect(),
            start: t_offset + start,
            end: t_offset + end,
        });
    }
    words
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

impl Synthesizer {
    pub fn new(g2p: G2p) -> Result<Self> {
        let dir = models_dir();
        let sess = Session::builder()?.commit_from_file(resolve_model(&dir)?)?;
        let voices = load_voices(&dir.join("voices-v1.0.bin"))?;
        Ok(Self { sess, voices, g2p, solo_cache: Default::default() })
    }

    /// Style vector for a voice at a given token count (row-indexed table).
    fn style(&self, voice: &str, n_tokens: usize) -> Result<Vec<f32>> {
        let row = |id: &str| -> Result<&[f32]> {
            let table = self.voices.get(id).with_context(|| format!("unknown voice {id}"))?;
            let rows = table.len() / 256;
            let r = n_tokens.min(rows - 1);
            Ok(&table[r * 256..(r + 1) * 256])
        };
        if let Some((_, recipe)) = CUSTOM_VOICES.iter().find(|(n, _)| *n == voice) {
            let mut acc = vec![0f32; 256];
            for (base, w) in recipe.iter() {
                for (a, x) in acc.iter_mut().zip(row(base)?) {
                    *a += w * x;
                }
            }
            return Ok(acc);
        }
        Ok(row(voice)?.to_vec())
    }

    fn run_model(&mut self, ids: &[i64], style: Vec<f32>, speed: f32) -> Result<(Vec<f32>, Vec<f32>)> {
        let mut padded = Vec::with_capacity(ids.len() + 2);
        padded.push(0);
        padded.extend_from_slice(ids);
        padded.push(0);
        let n = padded.len();
        let input_ids = Tensor::from_array(([1usize, n], padded))?;
        let style_t = Tensor::from_array(([1usize, 256], style))?;
        let speed_t = Tensor::from_array(([1usize], vec![speed]))?;
        let outputs = self.sess.run(ort::inputs![
            "input_ids" => input_ids,
            "style" => style_t,
            "speed" => speed_t,
        ])?;
        let (_, wav) = outputs["waveform"].try_extract_tensor::<f32>()?;
        let (_, dur) = outputs["durations"].try_extract_tensor::<f32>()?;
        Ok((wav.to_vec(), dur.to_vec()))
    }

    fn solo(&self, word: &str) -> String {
        let mut cache = self.solo_cache.borrow_mut();
        cache
            .entry(word.to_string())
            .or_insert_with(|| self.g2p.phonemize_solo(word).unwrap_or_default())
            .clone()
    }

    /// Map phoneme-words to text words. 1:1 when counts match; otherwise a
    /// monotonic DP alignment scored by edit distance against per-word solo
    /// phonemizations (espeak merges function words: "with the" → "wɪððə";
    /// and expands numerals: "42" → two phoneme-words).
    fn align(
        &self,
        pwords: &[PhonemeWord],
        twords: &[(usize, usize, &str)],
    ) -> Vec<WordTiming> {
        if pwords.is_empty() || twords.is_empty() {
            return Vec::new();
        }
        if pwords.len() == twords.len() {
            return pwords
                .iter()
                .zip(twords)
                .map(|(p, &(s, e, _))| WordTiming {
                    start: p.start,
                    end: p.end,
                    char_start: s,
                    char_end: e,
                })
                .collect();
        }
        let strip = |s: &str| -> String { s.chars().filter(|c| c.is_alphanumeric() || *c == 'ˈ' || *c == 'ˌ').collect() };
        let n = pwords.len();
        let m = twords.len();
        const BIG: usize = usize::MAX / 4;
        // dp[i][j]: min cost aligning pwords[i..] with twords[j..]
        let mut dp = vec![vec![BIG; m + 1]; n + 1];
        let mut mv = vec![vec![(0usize, 0usize); m + 1]; n + 1];
        dp[n][m] = 0;
        for i in (0..=n).rev() {
            for j in (0..=m).rev() {
                if i == n && j == m {
                    continue;
                }
                let mut best = BIG;
                let mut bmv = (0, 0);
                for (a, b) in [(1usize, 1usize), (1, 2), (1, 3), (2, 1), (3, 1)] {
                    if i + a > n || j + b > m || dp[i + a][j + b] >= BIG {
                        continue;
                    }
                    let p: String = pwords[i..i + a].iter().map(|w| strip(&w.phonemes)).collect();
                    let t: String = twords[j..j + b].iter().map(|&(_, _, w)| strip(&self.solo(w))).collect();
                    let cost = levenshtein(&p, &t) + (a + b - 2) * 2 + dp[i + a][j + b];
                    if cost < best {
                        best = cost;
                        bmv = (a, b);
                    }
                }
                dp[i][j] = best;
                mv[i][j] = bmv;
            }
        }
        let mut out = Vec::with_capacity(m);
        let (mut i, mut j) = (0usize, 0usize);
        while i < n && j < m {
            let (a, b) = mv[i][j];
            if a == 0 {
                break; // no valid path — handled by the completeness check below
            }
            let start = pwords[i].start;
            let end = pwords[i + a - 1].end;
            if b == 1 {
                let (s, e, _) = twords[j];
                out.push(WordTiming { start, end, char_start: s, char_end: e });
            } else {
                // one merged phoneme-word spans several text words: split its
                // time proportionally by solo phoneme lengths
                let lens: Vec<usize> =
                    twords[j..j + b].iter().map(|&(_, _, w)| self.solo(w).chars().count().max(1)).collect();
                let total: usize = lens.iter().sum();
                let mut t = start;
                for (k, &(s, e, _)) in twords[j..j + b].iter().enumerate() {
                    let frac = lens[k] as f32 / total as f32;
                    let t2 = if k == b - 1 { end } else { t + (end - start) * frac };
                    out.push(WordTiming { start: t, end: t2, char_start: s, char_end: e });
                    t = t2;
                }
            }
            i += a;
            j += b;
        }
        // if the DP couldn't map every text word (e.g. extreme merges), never
        // return empty — distribute the audio span proportionally instead
        if out.len() == m {
            out
        } else {
            self.proportional(pwords, twords)
        }
    }

    /// Spread the phoneme-words' total time span across the text words by their
    /// solo phoneme length. A graceful last resort when DP alignment fails.
    fn proportional(&self, pwords: &[PhonemeWord], twords: &[(usize, usize, &str)]) -> Vec<WordTiming> {
        let start = pwords.first().map_or(0.0, |p| p.start);
        let end = pwords.last().map_or(start, |p| p.end);
        let lens: Vec<usize> =
            twords.iter().map(|&(_, _, w)| self.solo(w).chars().count().max(1)).collect();
        let total: usize = lens.iter().sum::<usize>().max(1);
        let mut t = start;
        let mut out = Vec::with_capacity(twords.len());
        for (k, &(s, e, _)) in twords.iter().enumerate() {
            let t2 = if k == twords.len() - 1 {
                end
            } else {
                t + (end - start) * lens[k] as f32 / total as f32
            };
            out.push(WordTiming { start: t, end: t2, char_start: s, char_end: e });
            t = t2;
        }
        out
    }

    /// Synthesize one sentence/utterance. Returns raw f32 samples at 24 kHz
    /// plus word timings mapped to byte offsets of `text`.
    pub fn synthesize(&mut self, text: &str, voice: &str, speed: f32) -> Result<(Vec<f32>, Vec<WordTiming>)> {
        let phonemes = self.g2p.phonemize(text)?;
        if phonemes.is_empty() {
            bail!("no speakable content");
        }
        let mut audio: Vec<f32> = Vec::new();
        let mut pwords: Vec<PhonemeWord> = Vec::new();
        for batch in split_phonemes(&phonemes) {
            let mut ids = self.g2p.tokenize(&batch);
            let mut filtered = batch.clone();
            if ids.len() > MAX_PHONEME_LENGTH {
                ids.truncate(MAX_PHONEME_LENGTH);
                filtered = filtered.chars().take(MAX_PHONEME_LENGTH).collect();
            }
            if ids.is_empty() {
                continue;
            }
            let style = self.style(voice, ids.len())?;
            let (wav, dur) = self.run_model(&ids, style, speed)?;
            let t_offset = audio.len() as f32 / SAMPLE_RATE as f32;
            pwords.extend(walk_timestamps(&filtered, &dur, t_offset));
            audio.extend_from_slice(&wav);
        }
        if audio.is_empty() {
            bail!("no speakable content");
        }
        low_pass(&mut audio, SAMPLE_RATE as f32, lowpass_hz(voice));
        // merge punctuation-only phoneme-words into their neighbor
        let mut merged: Vec<PhonemeWord> = Vec::new();
        for pw in pwords {
            let lettered = pw.phonemes.chars().any(|c| c.is_alphanumeric());
            match (lettered, merged.last_mut()) {
                (false, Some(prev)) => prev.end = pw.end,
                (false, None) => merged.push(pw), // leading mark: keep, may merge forward
                (true, _) => {
                    if let Some(prev) = merged.last() {
                        if !prev.phonemes.chars().any(|c| c.is_alphanumeric()) {
                            let head = merged.pop().unwrap();
                            merged.push(PhonemeWord {
                                phonemes: pw.phonemes,
                                start: head.start,
                                end: pw.end,
                            });
                            continue;
                        }
                    }
                    merged.push(pw);
                }
            }
        }
        let twords: Vec<(usize, usize, &str)> = {
            let mut v = Vec::new();
            let bytes = text.as_bytes();
            let mut i = 0usize;
            while i < bytes.len() {
                while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                    i += 1;
                }
                if i >= bytes.len() {
                    break;
                }
                let st = i;
                while i < bytes.len() && !(bytes[i] as char).is_ascii_whitespace() {
                    i += 1;
                }
                v.push((st, i, std::str::from_utf8(&bytes[st..i]).unwrap_or("")));
            }
            v
        };
        Ok((audio, self.align(&merged, &twords)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mtts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn resolve_prefers_canonical_then_legacy() {
        let d = tmpdir("resolve");
        // only the legacy name — the MTTS_MODELS escape hatch
        std::fs::write(d.join("kokoro-ts.onnx"), [0u8; 8]).unwrap();
        assert!(resolve_model(&d).unwrap().ends_with("kokoro-ts.onnx"));
        // the canonical name wins; retired names are no longer resolvable
        std::fs::write(d.join("kokoro-int8.onnx"), [0u8; 8]).unwrap();
        std::fs::write(d.join("kokoro.onnx"), [0u8; 8]).unwrap();
        assert!(resolve_model(&d).unwrap().ends_with("kokoro.onnx"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_model_errors() {
        let d = tmpdir("empty");
        assert!(resolve_model(&d).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

        #[test]
    fn sentence_ranges_utf8_safe_on_long_persian() {
        // ~3.6 KB of Persian with ZWNJ and no ASCII sentence punctuation
        let fa = "این یک متن طولانی فارسی است که هیچ نقطه‌ای ندارد ".repeat(40);
        let ranges = sentence_ranges(&fa);
        assert!(!ranges.is_empty());
        assert!(ranges.len() > 1, "a >1200-byte run-on must hard-split");
        for &(a, b) in &ranges {
            assert!(fa.is_char_boundary(a) && fa.is_char_boundary(b));
            let _ = &fa[a..b]; // would panic pre-fix
        }
    }

        #[test]
    fn sentence_ranges_progress_on_spaceless_multibyte() {
        // em-dashes, no spaces, no ASCII punctuation — worst case for the cut
        let s = "—".repeat(1000);
        let ranges = sentence_ranges(&s);
        for &(a, b) in &ranges {
            assert!(s.is_char_boundary(a) && s.is_char_boundary(b));
            let _ = &s[a..b];
        }
    }

    // A word wider than the reader column wraps across lines; its highlight box
    // must stay on the first line (not span the full width on the wrong row).
}
