# Minimal TTS

A local read-aloud app. One window: paste text, press play, and follow along as
each sentence and word lights up in sync with the speech. No cloud, no accounts,
no telemetry — everything runs and stays on your machine.

https://github.com/user-attachments/assets/1c05f1bd-9c2d-42d6-b651-c3cae174cfff

- **[Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M)** — one of the top-rated
  open TTS models (Apache-2.0), 24 kHz output
- **Read-along** — the current sentence brightens and a highlight rectangle glides
  word-to-word, driven by the model's word timestamps
- **Sentence-streamed** — audio is generated as you listen and cached per
  (sentence, voice, speed), so playback starts instantly and seeks are free
- **28 voices + custom blends** — mix voice embeddings in one line of config

## Setup

Install [uv](https://docs.astral.sh/uv/), then:

```sh
git clone https://github.com/alirezazd/minimal-tts && cd minimal-tts
uv run main.py
```

uv handles Python and every dependency. A chromeless window opens at
`127.0.0.1:8765`; the first run downloads the model (~360 MB) into `./models/`,
and after that it's fully offline. Runs on NVIDIA (CUDA), Apple Silicon (MPS), or CPU.

**Optional launcher icon** — `./scripts/install.sh` (Linux) or `pwsh scripts/install.ps1`
(Windows). Reuses the Chrome you already have; no bundled browser.

## Voices

28 English voices; default is **Michael** (crisp US male). Voices are
style-embedding tensors, so you can blend your own in one line — e.g. **Chad**,
40% Puck + 60% Onyx:

```python
CUSTOM_VOICES = {
    "chad": {"label": "Chad", "group": "Custom",
             "recipe": [("am_puck", 0.4), ("am_onyx", 0.6)]},
}
```

## Configuration

Environment variables, all optional:

| Variable | Default | Purpose |
|---|---|---|
| `MINIMAL_TTS_PORT` | `8765` | Server port |
| `MINIMAL_TTS_HOST` | `127.0.0.1` | Set `0.0.0.0` to reach it from your LAN (no auth) |
| `MINIMAL_TTS_DEVICE` | auto | Force `cuda`, `mps`, or `cpu` |
| `MINIMAL_TTS_NO_BROWSER` | unset | Don't auto-open the window |
| `HF_HOME` | `./models` | Where model weights live |

## License

MIT. Speech model [Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M)
(Apache-2.0), G2P by [misaki](https://github.com/hexgrad/misaki).
