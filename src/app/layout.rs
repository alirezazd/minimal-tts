//! Text measuring, document geometry, and the aurora blob images.

use crate::synth::sentence_ranges;
use crate::WordUi;
use i_slint_core::textlayout::sharedparley::parley;

pub(crate) const FONT_PX: f32 = 16.0;
pub(crate) const LINE_H: f32 = 30.0;
pub(crate) const WORD_H: f32 = 19.0;
pub(crate) const PAD_X: f32 = 24.0; // must match the +24px/+22px offsets in app.slint
pub(crate) const PAD_Y: f32 = 22.0;
// ---------------------------------------------------------------- measuring

/// Metrics-patched Liberation Sans: 30px natural line advance at 16px,
/// glyphs vertically centered — the editor's TextInput and the reader share
/// the same rhythm because the font itself carries it.
pub(crate) const APP_FONT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/MinimalTTSSans.ttf"));
pub(crate) const APP_FONT_FAMILY: &str = "MinimalTTS Sans";

/// Make the bundled font visible to fontconfig (skia resolves by family).
pub(crate) fn install_app_font() {
    let dir = crate::synth::xdg_dir("XDG_DATA_HOME", ".local/share").join("fonts");
    let path = dir.join("MinimalTTSSans.ttf");
    let stale = std::fs::read(&path).map_or(true, |cur| cur != APP_FONT);
    if stale {
        let _ = std::fs::create_dir_all(&dir);
        if std::fs::write(&path, APP_FONT).is_ok() {
            let _ = std::process::Command::new("fc-cache").arg(&dir).status();
        }
    }
}

// ---------------------------------------------------------------- document

pub(crate) struct SentenceInfo {
    pub(crate) text: String,
    pub(crate) first_word: usize,
    pub(crate) n_words: usize,
    /// real audio length, memoized as the samples arrive; None until synthesized
    pub(crate) dur: Option<f32>,
}

pub(crate) struct Doc {
    pub(crate) sentences: Vec<SentenceInfo>,
    /// per ui-word: (sentence idx, word idx within sentence)
    pub(crate) word_of: Vec<(u32, u32)>,
    pub(crate) ui_words: Vec<WordUi>,
    /// per ui-word: byte range into `raw` (for Ctrl+F match → box mapping)
    pub(crate) word_range: Vec<(usize, usize)>,
    /// the laid-out (tidied) text the reader shows
    pub(crate) raw: String,
    pub(crate) height: f32,
}


pub(crate) fn word_ranges(raw: &str) -> Vec<(usize, usize)> {
    let bytes = raw.as_bytes();
    let mut v = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let st = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        v.push((st, i));
    }
    v
}

/// Lay the raw text out with Slint's own engine (parley), mirroring exactly
/// what the editor's TextInput does — so reader and editor are identical by
/// construction: same breaks, same lines, same glyph positions.
pub(crate) fn build_doc(raw: &str, logical_width: f32, scale: f32) -> Doc {
    use parley::style;

    let sents = sentence_ranges(raw);
    // words never cross a sentence boundary (fixes the "Hello.World" desync)
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut word_sent: Vec<usize> = Vec::new();
    for (si, &(a, b)) in sents.iter().enumerate() {
        for (ws, we) in word_ranges(&raw[a..b]) {
            words.push((a + ws, a + we));
            word_sent.push(si);
        }
    }

    let mut fcx = parley::FontContext::new();
    let mut lcx = parley::LayoutContext::<()>::new();
    let mut builder = lcx.ranged_builder(&mut fcx, raw, scale, false);
    // sharedparley pins line height to the font's own metrics ratio; for the
    // patched font that ratio is exactly 30px / 16px
    builder.push_default(parley::StyleProperty::LineHeight(
        style::LineHeight::FontSizeRelative(LINE_H / FONT_PX),
    ));
    let families = [
        style::FontFamilyName::named(APP_FONT_FAMILY),
        style::FontFamilyName::Generic(style::GenericFamily::SansSerif),
    ];
    builder.push_default(style::FontFamily::List(std::borrow::Cow::Borrowed(&families)));
    builder.push_default(parley::StyleProperty::FontSize(FONT_PX));
    builder.push_default(parley::StyleProperty::WordBreak(style::WordBreak::Normal));
    builder.push_default(parley::StyleProperty::OverflowWrap(style::OverflowWrap::Anywhere));
    let mut layout = builder.build(raw);
    layout.break_all_lines(Some(logical_width * scale));
    layout.align(parley::Alignment::Start, parley::AlignmentOptions::default());

    // word geometry from the engine's clusters (physical px -> logical)
    let mut geo: Vec<(f32, f32, f32)> = vec![(f32::MAX, f32::MIN, 0.0); words.len()]; // x0, x1, top
    let mut wi = 0usize;
    let mut y_top = 0.0f32;
    for line in layout.lines() {
        let lh = line.metrics().line_height;
        for item in line.items() {
            let parley::PositionedLayoutItem::GlyphRun(run) = item else { continue };
            let mut x = run.offset();
            for cluster in run.run().clusters() {
                let r = cluster.text_range();
                let adv = cluster.advance();
                while wi < words.len() && words[wi].1 <= r.start {
                    wi += 1;
                }
                if wi < words.len() && r.start >= words[wi].0 && r.start < words[wi].1 {
                    let g = &mut geo[wi];
                    // a word wider than the column wraps (OverflowWrap::Anywhere);
                    // lock its box to the first line so it can't span the full
                    // width on the wrong row
                    if g.0 == f32::MAX {
                        g.2 = y_top;
                    }
                    if (g.2 - y_top).abs() < 0.5 {
                        g.0 = g.0.min(x);
                        g.1 = g.1.max(x + adv);
                    }
                }
                x += adv;
            }
        }
        y_top += lh;
    }
    let doc_height = y_top / scale;

    // words -> sentences (each word already belongs to exactly one sentence)
    let mut infos: Vec<SentenceInfo> = sents
        .iter()
        .map(|&(a, b)| SentenceInfo {
            text: raw[a..b].to_string(),
            first_word: 0,
            n_words: 0,
            dur: None,
        })
        .collect();
    let mut word_of = Vec::with_capacity(words.len());
    let mut ui_words = Vec::with_capacity(words.len());
    for (k, &(a, b)) in words.iter().enumerate() {
        let si = word_sent[k];
        if infos[si].n_words == 0 {
            infos[si].first_word = k;
        }
        word_of.push((si as u32, infos[si].n_words as u32));
        infos[si].n_words += 1;
        let (x0, x1, top) = geo[k];
        let (x0, x1, top) = if x0 > x1 { (0.0, 0.0, 0.0) } else { (x0 / scale, x1 / scale, top / scale) };
        ui_words.push(WordUi {
            text: raw[a..b].into(),
            x: x0,
            // word box centered in the line cell, matching half-leading
            y: top + (LINE_H - WORD_H) * 0.5,
            w: x1 - x0,
            wbg: x1 - x0,
            h: WORD_H,
            sent: si as i32,
        });
    }
    // drop unreached trailing sentences (defensive) and extend tint bands
    while infos.last().map_or(false, |s| s.n_words == 0) {
        infos.pop();
    }
    for i in 0..ui_words.len().saturating_sub(1) {
        let (a, b) = (&ui_words[i], &ui_words[i + 1]);
        if a.sent == b.sent && (a.y - b.y).abs() < 0.5 {
            ui_words[i].wbg = b.x - a.x;
        }
    }
    Doc {
        sentences: infos,
        word_of,
        ui_words,
        word_range: words,
        raw: raw.to_string(),
        height: doc_height + 2.0 * PAD_Y + 24.0,
    }
}
pub(crate) fn doc_first_word(doc: &Doc, sent: u32) -> Option<usize> {
    doc.sentences.get(sent as usize).map(|s| s.first_word)
}

/// Soft radial blob texture. Falloff follows the tuned stop curve in float
/// precision, hits exact zero at 98% radius (nothing left for the edge to
/// clip), and is dithered while quantizing so the 8-bit result can't band.
pub(crate) fn make_blob(rgb: (u8, u8, u8), core_alpha: f32, stops: &[(f32, f32)]) -> slint::Image {
    const N: u32 = 768;
    // base curve compressed to 88% radius, then radially blurred — softer
    // ("more blur") while the tail still reaches zero inside the texture
    let base = |r: f32| -> f32 {
        let r = r / 0.88;
        if r >= stops[stops.len() - 1].0 {
            return 0.0;
        }
        let mut i = 0;
        while i + 1 < stops.len() && r >= stops[i + 1].0 {
            i += 1;
        }
        let (x0, y0) = stops[i];
        let (x1, y1) = stops[i + 1];
        y0 + (y1 - y0) * ((r - x0) / (x1 - x0))
    };
    const LUT_N: usize = 2048;
    let mut lut: Vec<f32> = (0..LUT_N).map(|i| base(i as f32 / (LUT_N - 1) as f32 * 1.5)).collect();
    let win = 60usize; // ±0.044 in r units, 3 box passes ≈ gaussian
    for _ in 0..3 {
        let src = lut.clone();
        for i in 0..LUT_N {
            let (a, b) = (i.saturating_sub(win), (i + win).min(LUT_N - 1));
            lut[i] = src[a..=b].iter().sum::<f32>() / (b - a + 1) as f32;
        }
    }
    let falloff = |r: f32| -> f32 {
        if r >= 0.98 {
            return 0.0;
        }
        lut[((r / 1.5) * (LUT_N - 1) as f32) as usize]
    };
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(N, N);
    let half = N as f32 / 2.0;
    let (cr, cg, cb) = (rgb.0 as f32 / 255.0, rgb.1 as f32 / 255.0, rgb.2 as f32 / 255.0);
    let mut seed: u32 = (rgb.0 as u32)
        .wrapping_mul(73_856_093)
        ^ (rgb.1 as u32).wrapping_mul(19_349_663)
        ^ (rgb.2 as u32).wrapping_mul(83_492_791)
        | 1;
    let px = buf.make_mut_slice();
    for y in 0..N {
        let dy = (y as f32 + 0.5) / half - 1.0;
        for x in 0..N {
            let dx = (x as f32 + 0.5) / half - 1.0;
            let a = falloff((dx * dx + dy * dy).sqrt()) * core_alpha;
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let n = (seed >> 8) as f32 / 16_777_216.0;
            // one noise sample for all channels keeps premultiplied invariants
            let q = |v: f32| (v * 255.0 + n).floor().clamp(0.0, 255.0) as u8;
            px[(y * N + x) as usize] = slint::Rgba8Pixel {
                r: q(cr * a),
                g: q(cg * a),
                b: q(cb * a),
                a: q(a),
            };
        }
    }
    slint::Image::from_rgba8_premultiplied(buf)
}

pub(crate) const BLOB_STOPS: [(f32, f32); 6] =
    [(0.0, 1.0), (0.28, 0.667), (0.53, 0.381), (0.76, 0.167), (0.88, 0.048), (0.98, 0.0)];
pub(crate) const DROP_STOPS: [(f32, f32); 5] =
    [(0.0, 1.0), (0.34, 0.582), (0.64, 0.261), (0.84, 0.091), (0.98, 0.0)];

#[cfg(test)]
mod tests {
    use super::*;

        #[test]
    fn wrapped_word_box_locked_to_first_line() {
        let long = "x".repeat(400); // far wider than the 200px column below
        let text = format!("{long} tail.");
        let doc = build_doc(&text, 200.0, 1.0);
        let w0 = &doc.ui_words[0];
        assert!(w0.y < LINE_H, "long word box y={} spilled to a lower line", w0.y);
        assert!(w0.w <= 201.0, "long word box w={} exceeds the column", w0.w);
    }
}
