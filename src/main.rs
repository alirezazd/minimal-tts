//! Offline TTS: a GUI when launched bare, a synthesis tool when given arguments.
//!
//!   minimal-tts [FILE|-|"text"] [-o OUT.wav|-] [--voice V] [--speed S] …
//!   cat article.txt | minimal-tts -o out.wav

mod app;
mod audio;
mod g2p;
mod synth;
mod tidy;

slint::include_modules!();

use anyhow::{bail, Context, Result};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

const USAGE: &str = "\
minimal-tts — offline neural text-to-speech

  minimal-tts                          launch the app
  minimal-tts FILE|-|\"text\" [options]  synthesize

Input is a file, `-` for stdin, or a literal string. Piped stdin is read
automatically.

Options
  -o, --out FILE|-   write WAV (`-` = stdout, for piping)
      --voice ID     voice to speak with (default am_michael)
      --speed S      0.5–2.0 (default 1.0)
      --play         play it aloud (the default with no other output)
      --tsv FILE     word timings: start, end, byte range, word
      --no-tidy      skip the PDF-paste cleanup applied by default
      --phonemes     print IPA per input line, no synthesis
      --tokens       print token ids per input line, no synthesis
      --list-voices  list voice ids
  -h, --help         this text
      --version      version

Examples
  minimal-tts article.txt --voice bm_george
  pdftotext paper.pdf - | minimal-tts -
  xclip -o | minimal-tts -o - | ffmpeg -i - clip.opus
";

#[derive(Default)]
struct Args {
    input: Option<String>, // positional: path, "-", or literal text
    out: Option<String>,
    voice: Option<String>,
    speed: Option<f32>,
    tsv: Option<String>,
    play: bool,
    no_tidy: bool,
    phonemes: bool,
    tokens: bool,
    list_voices: bool,
    help: bool,
    version: bool,
}

/// Sequential parse: unknown flags and missing values are errors, `--` ends
/// option parsing. (The old predicate-based scan picked a flag's *value* as the
/// positional, so `--out x.wav article.txt` never opened the article.)
fn parse_args(argv: Vec<String>) -> Result<Args> {
    let mut a = Args::default();
    let mut it = argv.into_iter();
    let mut rest_positional = false;
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| -> Result<String> {
            it.next()
                .filter(|v| v == "-" || !v.starts_with('-'))
                .with_context(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            _ if rest_positional => a.input = Some(arg),
            "--" => rest_positional = true,
            "-o" | "--out" => a.out = Some(value("--out")?),
            "--voice" => a.voice = Some(value("--voice")?),
            "--speed" => {
                let raw = value("--speed")?;
                let s: f32 = raw.parse().with_context(|| format!("--speed {raw}: not a number"))?;
                if !s.is_finite() || !(0.5..=2.0).contains(&s) {
                    bail!("--speed {raw}: must be between 0.5 and 2.0");
                }
                a.speed = Some(s);
            }
            "--tsv" => a.tsv = Some(value("--tsv")?),
            "--play" => a.play = true,
            "--no-tidy" => a.no_tidy = true,
            "--phonemes" => a.phonemes = true,
            "--tokens" => a.tokens = true,
            "--list-voices" => a.list_voices = true,
            "-h" | "--help" => a.help = true,
            "--version" => a.version = true,
            "-" => a.input = Some(arg),
            _ if arg.starts_with('-') => bail!("unknown option {arg}\n\n{USAGE}"),
            _ if a.input.is_some() => bail!("unexpected extra argument {arg}"),
            _ => a.input = Some(arg),
        }
    }
    Ok(a)
}

const MAX_INPUT_BYTES: u64 = 2 << 20;

/// Normalize text for synthesis: strip a UTF-8 BOM (it would become a leading
/// pseudo-word and shift every byte offset) and fold CRLF.
fn normalize(mut text: String) -> String {
    if text.starts_with('\u{feff}') {
        text.remove(0);
    }
    if text.contains('\r') {
        text = text.replace("\r\n", "\n").replace('\r', "\n");
    }
    text
}

fn read_capped(mut r: impl Read, source: &str) -> Result<String> {
    let mut buf = Vec::new();
    r.by_ref().take(MAX_INPUT_BYTES + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_INPUT_BYTES {
        bail!("{source}: larger than {} MB", MAX_INPUT_BYTES / (1 << 20));
    }
    let s = String::from_utf8(buf).map_err(|_| anyhow::anyhow!("{source}: not UTF-8 text"))?;
    Ok(normalize(s))
}

fn read_file(path: &Path) -> Result<String> {
    let meta = std::fs::metadata(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    if meta.is_dir() {
        bail!("{} is a directory", path.display());
    }
    let f = std::fs::File::open(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    read_capped(f, &path.display().to_string())
}

/// True when stdin is a pipe or a redirected file — i.e. someone piped us data.
/// A terminal, or the /dev/null a desktop launcher hands over, is neither.
fn stdin_has_data() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        std::fs::metadata("/dev/stdin")
            .map(|m| m.file_type().is_fifo() || m.file_type().is_file())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        !std::io::stdin().is_terminal()
    }
}

/// Resolve the positional into text. A path-looking argument that doesn't exist
/// is an error, not something to read aloud.
fn resolve_input(arg: Option<&str>) -> Result<String> {
    match arg {
        Some("-") | None => read_capped(std::io::stdin().lock(), "stdin"),
        Some(s) => {
            let p = Path::new(s);
            if p.exists() {
                return read_file(p);
            }
            let looks_like_path = s.contains('/')
                || matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("txt" | "md" | "text" | "log" | "csv" | "json")
                );
            if looks_like_path {
                bail!("{s}: no such file");
            }
            Ok(normalize(s.to_string()))
        }
    }
}

/// Where audio goes. Files land via a `.part` rename so a mid-run failure can't
/// truncate the target (or the input, if they're the same path).
enum Sink {
    File(hound::WavWriter<std::io::BufWriter<std::fs::File>>, PathBuf, PathBuf),
    Stdout(std::io::BufWriter<std::io::Stdout>),
    None,
}

impl Sink {
    fn open(out: Option<&str>, input: Option<&Path>) -> Result<Self> {
        let Some(out) = out else { return Ok(Sink::None) };
        if out == "-" {
            if std::io::stdout().is_terminal() {
                bail!("refusing to write WAV to a terminal — redirect it, e.g. `-o - | ffmpeg -i - out.opus`");
            }
            let mut w = std::io::BufWriter::new(std::io::stdout());
            // streaming RIFF: unknown length, which players accept from a pipe
            w.write_all(b"RIFF")?;
            w.write_all(&u32::MAX.to_le_bytes())?;
            w.write_all(b"WAVEfmt ")?;
            w.write_all(&16u32.to_le_bytes())?;
            w.write_all(&1u16.to_le_bytes())?; // PCM
            w.write_all(&1u16.to_le_bytes())?; // mono
            w.write_all(&synth::SAMPLE_RATE.to_le_bytes())?;
            w.write_all(&(synth::SAMPLE_RATE * 2).to_le_bytes())?; // byte rate
            w.write_all(&2u16.to_le_bytes())?; // block align
            w.write_all(&16u16.to_le_bytes())?;
            w.write_all(b"data")?;
            w.write_all(&u32::MAX.to_le_bytes())?;
            return Ok(Sink::Stdout(w));
        }
        let dest = PathBuf::from(out);
        if dest.is_dir() {
            bail!("{}: is a directory", dest.display());
        }
        if !dest.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")) {
            bail!(
                "{}: only .wav is written directly — for other formats pipe it, \
                 e.g. `-o - | ffmpeg -i - out.opus`",
                dest.display()
            );
        }
        if let (Some(i), Ok(d)) = (input, dest.canonicalize()) {
            if i.canonicalize().ok().as_deref() == Some(&d) {
                bail!("{}: output would overwrite the input", dest.display());
            }
        }
        let part = dest.with_extension("wav.part");
        let f = std::fs::File::create(&part)
            .map_err(|e| anyhow::anyhow!("{}: {e}", part.display()))?;
        let w = hound::WavWriter::new(std::io::BufWriter::new(f), synth::wav_spec())?;
        Ok(Sink::File(w, part, dest))
    }

    fn write(&mut self, samples: &[f32]) -> Result<()> {
        match self {
            Sink::File(w, ..) => {
                for &s in samples {
                    w.write_sample(synth::pcm16(s))?;
                }
            }
            Sink::Stdout(w) => {
                for &s in samples {
                    w.write_all(&synth::pcm16(s).to_le_bytes())?;
                }
            }
            Sink::None => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<()> {
        match self {
            Sink::File(w, part, dest) => {
                w.finalize()?;
                std::fs::rename(&part, &dest)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", dest.display()))?;
                eprintln!("wrote {}", dest.display());
            }
            Sink::Stdout(mut w) => w.flush()?,
            Sink::None => {}
        }
        Ok(())
    }

    fn abandon(self) {
        if let Sink::File(w, part, _) = self {
            drop(w);
            let _ = std::fs::remove_file(part);
        }
    }
}

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    // bare launch with nothing piped in → the app
    if argv.is_empty() && !stdin_has_data() {
        return app::run();
    }
    let args = parse_args(argv)?;
    if args.help {
        print!("{USAGE}");
        return Ok(());
    }
    if args.version {
        println!("minimal-tts {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.list_voices {
        // straight from the static table: works with no models installed
        for (group, ids) in synth::VOICE_GROUPS {
            println!("{group}:");
            for id in *ids {
                let mark = if *id == synth::DEFAULT_VOICE { " (default)" } else { "" };
                println!("  {id}{mark}");
            }
        }
        return Ok(());
    }

    let voice: &str = args.voice.as_deref().unwrap_or(synth::DEFAULT_VOICE);
    if !synth::voice_ids().any(|v| v == voice) {
        // checked before the model loads, so a typo fails in milliseconds
        match synth::nearest_voice(voice) {
            Some(near) => bail!("unknown voice {voice} — did you mean {near}?"),
            None => bail!("unknown voice {voice} — see --list-voices"),
        }
    }
    let speed = args.speed.unwrap_or(1.0);

    let input_path = args.input.as_deref().map(Path::new);
    let raw = resolve_input(args.input.as_deref())?;

    let g2p = g2p::G2p::new()?;
    if g2p.lexicon_len() > 0 && std::io::stderr().is_terminal() {
        // a lexicon that silently isn't applied is the failure mode worth naming
        eprintln!("{} pronunciation overrides from {}", g2p.lexicon_len(), g2p::lexicon_path().display());
    }

    // dump modes stay per-line: tests/corpus.txt is a line-per-utterance oracle
    if args.phonemes || args.tokens {
        if args.out.as_deref() == Some("-") {
            bail!("--phonemes/--tokens print to stdout; they can't share it with `-o -`");
        }
        let stdout = std::io::stdout();
        let mut w = std::io::BufWriter::new(stdout.lock());
        for line in raw.lines().filter(|l| !l.trim().is_empty()) {
            let ph = g2p.phonemize(line)?;
            let out = if args.phonemes {
                ph
            } else {
                g2p.tokenize(&ph).iter().map(|i| i.to_string()).collect::<Vec<_>>().join(" ")
            };
            // a closed pipe (`| head`) is a clean exit, not a panic
            if writeln!(w, "{out}").is_err() {
                return Ok(());
            }
        }
        let _ = w.flush();
        return Ok(());
    }

    let text = if args.no_tidy || !tidy::looks_messy(&raw) {
        raw
    } else {
        eprintln!("note: auto-tidied the input (--no-tidy to keep it verbatim); \
                   --tsv offsets refer to the tidied text");
        tidy::tidy(&raw)
    };

    let sentences = synth::sentence_ranges(&text);
    if sentences.is_empty() {
        bail!("nothing to speak");
    }

    let want_play = args.play || (args.out.is_none() && args.tsv.is_none());
    let mut sink = Sink::open(args.out.as_deref(), input_path)?;
    let engine = match want_play {
        false => None,
        true => match audio::Audio::new() {
            Ok(e) => Some(e),
            // a file was still requested: produce it rather than dying on audio
            Err(e) if !matches!(sink, Sink::None) => {
                eprintln!("note: no audio output ({e}) — writing the file only");
                None
            }
            Err(e) => return Err(e),
        },
    };

    let mut synth = synth::Synthesizer::new(g2p)?;
    let progress = std::io::stderr().is_terminal();
    let t0 = std::time::Instant::now();
    let mut total_samples: u64 = 0;
    let mut timings: Vec<synth::WordTiming> = Vec::new();
    let mut spoken = 0usize;
    let mut epoch = 0u64;
    // per-sentence words for the read-along: the audio clock reports time
    // *within* a segment, so document-rebased times can't drive it
    let mut playing: Vec<(String, Vec<synth::WordTiming>)> = Vec::new();
    let mut ra = ReadAlong::default();

    for (i, &(a, b)) in sentences.iter().enumerate() {
        let chunk = &text[a..b];
        let (samples, words) = match synth.synthesize(chunk, voice, speed) {
            Ok(v) => v,
            // matches the app: an unspeakable sentence is skipped, not fatal
            Err(e) if e.to_string().contains("no speakable content") => continue,
            Err(e) => {
                sink.abandon();
                return Err(e);
            }
        };
        sink.write(&samples)?;
        if args.tsv.is_some() {
            let offset = total_samples as f32 / synth::SAMPLE_RATE as f32;
            timings.extend(words.iter().map(|w| synth::WordTiming {
                start: w.start + offset,
                end: w.end + offset,
                char_start: a + w.char_start,
                char_end: a + w.char_end,
            }));
        }
        total_samples += samples.len() as u64;
        spoken += 1;

        if let Some(engine) = engine.as_ref() {
            let seg = audio::Segment {
                sentence: i as u32,
                offset: 0,
                data: std::sync::Arc::new(samples),
            };
            playing.push((chunk.to_string(), words));
            if playing.len() == 1 {
                epoch = engine.play_now(seg);
            } else {
                engine.enqueue(epoch, seg);
            }
            // stay a few sentences ahead so the queue can't grow unbounded
            while engine.position().is_some_and(|(s, _)| (i as u32).saturating_sub(s) >= 8) {
                read_along(engine, &playing, &mut ra);
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            read_along(engine, &playing, &mut ra);
        } else if progress {
            eprint!("\rsynthesizing {}/{}", i + 1, sentences.len());
        }
    }
    if progress && engine.is_none() {
        eprint!("\r\x1b[K");
    }
    if spoken == 0 {
        sink.abandon();
        bail!("nothing speakable in the input");
    }

    sink.finish()?;

    if let Some(path) = args.tsv.as_deref() {
        use std::fmt::Write as _;
        let mut out = String::with_capacity(timings.len() * 40);
        for w in &timings {
            let _ = writeln!(
                out,
                "{:.3}\t{:.3}\t{}\t{}\t{}",
                w.start, w.end, w.char_start, w.char_end, &text[w.char_start..w.char_end]
            );
        }
        std::fs::write(path, out).map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
        eprintln!("wrote {path}");
    }

    if let Some(engine) = engine.as_ref() {
        // everything is queued now, so a drained clock really means "finished"
        while engine.position().is_some() {
            read_along(engine, &playing, &mut ra);
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        // let the device drain what it already has buffered
        std::thread::sleep(std::time::Duration::from_millis(150));
        eprintln!();
    }

    let dur = total_samples as f32 / synth::SAMPLE_RATE as f32;
    if progress {
        let wall = t0.elapsed().as_secs_f32();
        eprintln!(
            "{dur:.2}s audio from {spoken} sentence(s), {wall:.2}s wall ({:.1}x realtime)",
            dur / wall.max(1e-6)
        );
    }
    Ok(())
}

/// Print words as the audio clock reaches them. `position()` reports time
/// *within* the current segment, so each sentence keeps its own word cursor.
#[derive(Default)]
struct ReadAlong {
    sent: Option<u32>,
    word: usize,
}

fn read_along(
    engine: &audio::Audio,
    playing: &[(String, Vec<synth::WordTiming>)],
    st: &mut ReadAlong,
) {
    let Some((sent, secs)) = engine.position() else { return };
    let Some((chunk, words)) = playing.get(sent as usize) else { return };
    if st.sent != Some(sent) {
        st.sent = Some(sent);
        st.word = 0;
    }
    while st.word < words.len() && words[st.word].start as f64 <= secs {
        let w = &words[st.word];
        eprint!("{} ", &chunk[w.char_start..w.char_end]);
        let _ = std::io::stderr().flush();
        st.word += 1;
    }
}
