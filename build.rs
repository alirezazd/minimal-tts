use std::path::Path;

// Slint has no line-height property: line rhythm comes from font metrics.
// Patch Liberation Sans so its natural line advance at 16px equals the
// reader's 30px cells (1.875em), with symmetric ascent/descent so glyphs
// sit centered like CSS line-height. Renamed with equal-length strings so
// no table offsets move (SIL OFL: renamed derivative).
fn patch_font() {
    let src = std::fs::read("assets/LiberationSans-Regular.ttf").expect("vendored font");
    let mut d = src.clone();

    let n = u16::from_be_bytes([d[4], d[5]]) as usize;
    let table = |tag: &[u8; 4]| -> usize {
        (0..n)
            .map(|i| 12 + 16 * i)
            .find(|&off| &d[off..off + 4] == tag)
            .map(|off| u32::from_be_bytes([d[off + 8], d[off + 9], d[off + 10], d[off + 11]]) as usize)
            .unwrap_or_else(|| panic!("table {tag:?} not found"))
    };
    let hhea = table(b"hhea");
    let os2 = table(b"OS/2");
    let head = table(b"head");

    let upem = u16::from_be_bytes([d[head + 18], d[head + 19]]) as i32;
    // 30px lines at 16px em — and ascent/descent snapped to whole pixels at
    // 16px (multiples of upem/16), so renderer rounding can't drift the
    // advance (Skia ceils fractional metrics: 20.55px ascent became 21px).
    let unit_per_px = upem / 16;
    let target = 30 * unit_per_px;
    let asc = i16::from_be_bytes([d[hhea + 4], d[hhea + 5]]) as i32;
    let desc = i16::from_be_bytes([d[hhea + 6], d[hhea + 7]]) as i32;
    let extra = target - (asc - desc);
    let na_raw = asc + extra / 2;
    let na = ((na_raw + unit_per_px / 2) / unit_per_px * unit_per_px) as i16;
    let nd = -((target - na as i32) as i16);

    let put16 = |d: &mut Vec<u8>, off: usize, v: i16| d[off..off + 2].copy_from_slice(&v.to_be_bytes());
    put16(&mut d, hhea + 4, na);
    put16(&mut d, hhea + 6, nd);
    put16(&mut d, hhea + 8, 0); // lineGap
    put16(&mut d, os2 + 68, na); // sTypoAscender
    put16(&mut d, os2 + 70, nd); // sTypoDescender
    put16(&mut d, os2 + 72, 0); // sTypoLineGap
    put16(&mut d, os2 + 74, na); // usWinAscent
    put16(&mut d, os2 + 76, -(nd as i32) as i16); // usWinDescent (positive)
    let fs = u16::from_be_bytes([d[os2 + 62], d[os2 + 63]]) | 0x80; // USE_TYPO_METRICS
    d[os2 + 62..os2 + 64].copy_from_slice(&fs.to_be_bytes());

    // equal-length renames, ASCII and UTF-16BE
    for (from, to) in [
        (&b"Liberation Sans"[..], &b"MinimalTTS Sans"[..]),
        (&b"LiberationSans"[..], &b"MinimalTTSSans"[..]),
    ] {
        replace_all(&mut d, from, to);
        let wide = |s: &[u8]| -> Vec<u8> { s.iter().flat_map(|&b| [0u8, b]).collect() };
        replace_all(&mut d, &wide(from), &wide(to));
    }

    let out = std::env::var("OUT_DIR").unwrap();
    std::fs::write(Path::new(&out).join("MinimalTTSSans.ttf"), &d).unwrap();
    println!("cargo:rerun-if-changed=assets/LiberationSans-Regular.ttf");
}

fn replace_all(d: &mut Vec<u8>, from: &[u8], to: &[u8]) {
    assert_eq!(from.len(), to.len());
    let mut i = 0;
    while i + from.len() <= d.len() {
        if &d[i..i + from.len()] == from {
            d[i..i + from.len()].copy_from_slice(to);
            i += from.len();
        } else {
            i += 1;
        }
    }
}

fn main() {
    patch_font();
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/app.slint", config).unwrap();
}
