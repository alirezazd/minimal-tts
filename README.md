# Minimal TTS

A local read-aloud app. Paste text, press play, and follow along as each
sentence and word lights up in sync with the speech. No cloud, no accounts, no
telemetry — everything runs and stays on your machine.

https://github.com/user-attachments/assets/1c05f1bd-9c2d-42d6-b651-c3cae174cfff

Native **Rust** + [Slint](https://slint.dev) — no Python, no browser, no runtime
dependencies. [Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M) (Apache-2.0)
runs on ONNX Runtime with espeak-ng for G2P, and word timings come from the
model's own duration predictions, not estimates.

- **Read-along** — the current sentence brightens and a highlight glides
  word-to-word, driven by the model's word timestamps
- **Sentence-streamed** — audio is generated as you listen and cached per
  (sentence, voice, speed), so playback starts instantly and seeks are free
- **28 voices plus your own blends**, and a CLI for scripting or batch export
- **Offline** — the model ships with the app; nothing is ever downloaded

## Install (Linux)

Grab the `.AppImage` from the
[latest release](https://github.com/alirezazd/minimal-tts/releases/latest), then:

```sh
chmod +x Minimal_TTS-*-x86_64.AppImage
./Minimal_TTS-*-x86_64.AppImage
```

Opening it with [Gear Lever](https://github.com/mijorus/gearlever) adds an
app-menu entry and keeps it updated from GitHub releases.

## CLI

Launched bare it opens the app; given a file, `-`, or a literal string it
synthesizes. Piped input is picked up automatically. `--help` lists everything.

```sh
minimal-tts article.txt                               # read it aloud
minimal-tts article.txt -o out.wav --voice bm_george --speed 1.2
pdftotext paper.pdf - | minimal-tts -                 # anything that emits text
xclip -o | minimal-tts -o - | ffmpeg -i - clip.opus   # stdout for other formats
minimal-tts article.txt -o out.wav --tsv words.tsv    # word timings
minimal-tts --list-voices
```

Only `.wav` is written directly — `-o -` streams it to stdout, so ffmpeg or sox
covers every other format. Synthesis runs sentence by sentence, so document
length costs time, not memory.

## Build from source

```sh
sudo dnf install alsa-lib-devel espeak-ng   # build: ALSA headers; runtime: espeak-ng
./scripts/get-models.sh                     # model + voices (~350 MB)
cargo run --release
./scripts/build-appimage.sh                 # -> dist/
```

## Configuration

| Variable | Effect |
|---|---|
| `MTTS_MODELS` | Model directory (overrides the bundled lookup) |
| `MTTS_LOWPASS` | Low-pass cutoff in Hz (`0` disables) |
| `MTTS_ESPEAK_LIB` / `MTTS_ESPEAK_DATA` | Explicit espeak-ng library / data paths |

- State (text, voice, speed, resume position) → `~/.config/minimal-tts/state.json`
- Pronunciation overrides → `~/.config/minimal-tts/lexicon.tsv`
- Audio exports → `~/Downloads`

### Voices

28 English voices, default **Michael** (crisp US male); `--list-voices` prints
them alongside any blends you define. Voices are style-embedding tensors, so
blends are one line in `src/synth.rs` — e.g. **Chad**, 40% Puck + 60% Onyx:

```rust
pub const CUSTOM_VOICES: &[(&str, &[(&str, f32)])] =
    &[("chad", &[("am_puck", 0.4), ("am_onyx", 0.6)])];
```

### Pronunciation

Names, acronyms and jargon that espeak mangles can be respelled once in
`~/.config/minimal-tts/lexicon.tsv` — one `term<TAB>respelling` per line:

```tsv
kubectl	koob cuttle
AWS	ay double you ess
```

Plain English, no phonetic alphabet. Matching is whole-word and
case-insensitive, and only pronunciation changes — your text and the read-along
highlighting are untouched.

## Notes

- **Pasting PDF text** cleans it up in place — joined hyphenation, unwrapped
  line breaks, dropped citation markers — so what you see is what gets read.
  Ctrl+Z reverts it, Ctrl+Y redoes it.
- **Ctrl+F** finds text in both the editor and the read-along view.
- Output is low-passed to drop model hiss — 8 kHz for male voices, 11 kHz for
  female so sibilance survives.

## License

MIT. Speech model [Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M)
(Apache-2.0), G2P by [espeak-ng](https://github.com/espeak-ng/espeak-ng)
(GPL-3.0, loaded at runtime), bundled font Liberation Sans (SIL OFL).
