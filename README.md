# Minimal TTS

A local, high-quality read-aloud app. One glass window: paste text, press play,
watch it read to you — sentence and word highlighted in sync with the speech.
No cloud, no accounts, no telemetry. Everything runs and stays on your machine.

<!-- DEMO VIDEO: to embed an inline player with audio, edit this file on github.com,
     delete this comment and the link line below, then drag assets/demo.mp4 into the
     editor on this spot — GitHub uploads it and inserts a <video> player. -->
**▶ [Watch the demo — with audio](assets/demo.mp4)**

- **[Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M)** — one of the
  top-rated open TTS models (Apache-2.0), 24 kHz output
- **Read-along** — the current sentence brightens and a highlight rectangle
  glides word-to-word, driven by the model's word timestamps
- **Sentence-streamed** — audio is generated as you listen and cached per
  (sentence, voice, speed), so playback starts instantly and seeks are free
- **28 voices + custom blends** — mix voice embeddings in one line of config
- **Private by construction** — binds to `127.0.0.1`; after the first model
  download it works fully offline

## Quickstart

Requires [uv](https://docs.astral.sh/uv/getting-started/installation/) (it
manages Python and all dependencies for you):

```sh
git clone https://github.com/alirezazd/minimal-tts && cd minimal-tts
uv run main.py
```

The browser opens at `http://127.0.0.1:8765`. The first run downloads the model
and voices (~360 MB) into `./models/`; after that, no network needed.

Works on **NVIDIA GPUs** (CUDA, ~90× realtime), **Apple Silicon** (MPS), and
plain **CPU** (~3–4× realtime — still comfortably ahead of playback thanks to
sentence streaming). No system packages needed — espeak-ng ships bundled via
Python wheels.

## Using it

One window, two modes:

- **Edit** (stopped) — type or paste freely. **Ctrl+Enter** or ▶ starts reading.
  A **Tidy** pill appears on messy PDF pastes: it joins hyphen-broken words,
  strips `[n]` citations, and unwraps hard line breaks (click again to undo).
- **Read** (playing/paused) — the text locks into a read-along view.
  Click any sentence or word to jump there. **Space** pauses,
  **← →** jump sentences, **Esc** (or ■) returns to editing with your caret at
  the last-read sentence.
- **Resume** — reading the same text again continues where you stopped.
- **Media keys** — play/pause/next/prev from your keyboard or the OS media overlay.
- **Speed** — applies live from the next sentence; cached sentences replay instantly.
- **Download** — exports Opus by default (~10× smaller than WAV); Shift-click for WAV.

Your text, voice, and speed are remembered between visits (locally).

## Voices

All 28 English Kokoro voices, grouped US/UK × male/female, best-rated first.
Default is **Michael** (crisp US male); try **Fenrir** (energetic), **George**
(measured British), **Onyx** (deep), or **Heart**/**Bella** (top-rated female).

**Chad** is a custom voice — a 40/60 neural blend of Puck and Onyx. Voices are
style-embedding tensors, so you can invent your own blends in `main.py`:

```python
CUSTOM_VOICES = {
    "chad": {"label": "Chad", "group": "Custom",
             "recipe": [("am_puck", 0.4), ("am_onyx", 0.6)]},
}
```

Other languages (ja, zh, es, fr, …) exist in Kokoro — extend `VOICE_GROUPS`
and see the [voice list](https://huggingface.co/hexgrad/Kokoro-82M/tree/main/voices).

## Configuration

Environment variables, all optional:

| Variable | Default | Purpose |
|---|---|---|
| `MINIMAL_TTS_PORT` | `8765` | Server port |
| `MINIMAL_TTS_HOST` | `127.0.0.1` | Set `0.0.0.0` to reach it from your LAN/phone (no auth — trust your network) |
| `MINIMAL_TTS_DEVICE` | auto | Force `cuda`, `mps`, or `cpu` |
| `MINIMAL_TTS_NO_BROWSER` | unset | Set to skip auto-opening the browser |
| `HF_HOME` | `./models` | Where model weights live |

## How it works

`main.py` (~270 lines) is a FastAPI server wrapping Kokoro on PyTorch.
The page splits your text into sentences (`Intl.Segmenter`) and requests each
from `/api/sentence`, which returns base64 WAV plus per-word timestamps mapped
to character offsets; the client schedules gapless playback through Web Audio
and drives the highlights from the audio clock. A server-side LRU cache keyed
by (text, voice, speed) makes replays and speed flips instant. `index.html`
(~800 lines, no build step, no dependencies) is the entire UI.

## Troubleshooting

- **Port in use** — set `MINIMAL_TTS_PORT`.
- **First request returns 503** — the model is still loading; it resolves in seconds.
- **GPU misbehaving** — `MINIMAL_TTS_DEVICE=cpu uv run main.py`.
- **First run is slow to start** — it's the one-time ~360 MB model download; watch
  progress in the terminal.

## Credits & license

MIT. Speech model: [Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M)
by hexgrad (Apache-2.0), with G2P by [misaki](https://github.com/hexgrad/misaki).
