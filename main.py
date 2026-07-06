"""Minimal TTS — a local read-aloud server with a single-page UI.

Run:  uv run main.py   (opens http://127.0.0.1:8765 in your browser)
"""

import base64
import io
import logging
import os
import threading
import webbrowser
from collections import OrderedDict
from pathlib import Path

PROJECT_DIR = Path(__file__).resolve().parent
# Keep model weights inside the project instead of ~/.cache — must be set
# before anything imports huggingface_hub.
os.environ.setdefault("HF_HOME", str(PROJECT_DIR / "models"))

import numpy as np
import soundfile as sf
import torch
import uvicorn
from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse, Response
from pydantic import BaseModel, Field

logging.basicConfig(level=logging.INFO, format="%(levelname)s: %(message)s")
log = logging.getLogger("minimal-tts")

HOST = os.environ.get("MINIMAL_TTS_HOST", "127.0.0.1")
PORT = int(os.environ.get("MINIMAL_TTS_PORT", "8765"))


def _pick_device() -> str:
    if torch.cuda.is_available():
        return "cuda"
    if getattr(torch.backends, "mps", None) and torch.backends.mps.is_available():
        return "mps"
    return "cpu"


DEVICE = os.environ.get("MINIMAL_TTS_DEVICE") or _pick_device()
SAMPLE_RATE = 24_000
ENGINE_NAME = "Kokoro-82M"
GAIN = 1.5  # output loudness boost, clipped to [-1, 1]
MAX_CHARS = 20_000

# All English Kokoro voices, best-rated first within each group.
# The first letter of the id picks the language pipeline: a=US, b=UK.
VOICE_GROUPS = {
    "US male": [
        "am_michael", "am_fenrir", "am_puck", "am_echo", "am_eric",
        "am_liam", "am_onyx", "am_adam", "am_santa",
    ],
    "UK male": ["bm_george", "bm_fable", "bm_lewis", "bm_daniel"],
    "US female": [
        "af_heart", "af_bella", "af_nicole", "af_aoede", "af_kore",
        "af_sarah", "af_nova", "af_alloy", "af_sky", "af_jessica", "af_river",
    ],
    "UK female": ["bf_emma", "bf_isabella", "bf_alice", "bf_lily"],
}
# Custom voices: weighted blends of preset voice tensors. Tweak the weights
# to taste — more am_onyx = deeper.
CUSTOM_VOICES = {
    "chad": {"label": "Chad", "group": "Custom",
             "recipe": [("am_puck", 0.4), ("am_onyx", 0.6)]},
}
VOICES = {
    **{vid: (spec["label"], spec["group"]) for vid, spec in CUSTOM_VOICES.items()},
    **{vid: (vid.split("_", 1)[1].capitalize(), group)
       for group, ids in VOICE_GROUPS.items()
       for vid in ids},
}
DEFAULT_VOICE = "am_michael"

_lock = threading.Lock()
_model = None
_pipelines: dict = {}
_blends: dict = {}


def _get_pipeline(lang_code: str):
    """Lazily build one KModel and share it across per-language pipelines."""
    global _model
    from kokoro import KModel, KPipeline

    if _model is None:
        log.info("Loading %s onto %s …", ENGINE_NAME, DEVICE)
        _model = KModel(repo_id="hexgrad/Kokoro-82M").to(DEVICE).eval()
    if lang_code not in _pipelines:
        _pipelines[lang_code] = KPipeline(
            lang_code=lang_code, model=_model, repo_id="hexgrad/Kokoro-82M"
        )
    return _pipelines[lang_code]


def _resolve_voice(voice: str):
    """Map a voice id to (lang_code, name-or-blended-tensor)."""
    if voice in CUSTOM_VOICES:
        recipe = CUSTOM_VOICES[voice]["recipe"]
        lang = recipe[0][0][0]
        if voice not in _blends:
            pipe = _get_pipeline(lang)
            _blends[voice] = sum(w * pipe.load_voice(base) for base, w in recipe)
        return lang, _blends[voice]
    return voice[0], voice


def _generate(text: str, voice: str, speed: float, with_words: bool = False):
    """Synthesize text; optionally collect word timings mapped to char offsets."""
    lang, resolved = _resolve_voice(voice)
    pipeline = _get_pipeline(lang)
    chunks, words = [], []
    offset, cursor = 0.0, 0
    with torch.inference_mode():
        for result in pipeline(text, voice=resolved, speed=speed):
            if result.audio is None:
                continue
            audio = result.audio.detach().cpu().numpy()
            if with_words:
                for tk in result.tokens or []:
                    ts = getattr(tk, "start_ts", None)
                    te = getattr(tk, "end_ts", None)
                    if ts is None or te is None:
                        continue
                    pos = text.find(tk.text, cursor)
                    if pos == -1:  # G2P normalized the token away — skip highlight
                        continue
                    words.append({
                        "t": tk.text,
                        "s": round(offset + ts, 3),
                        "e": round(offset + te, 3),
                        "cs": pos,
                        "ce": pos + len(tk.text),
                    })
                    cursor = pos + len(tk.text)
            offset += len(audio) / SAMPLE_RATE
            chunks.append(audio)
    if not chunks:
        raise ValueError("no speakable content")
    full = np.clip(np.concatenate(chunks) * GAIN, -1.0, 1.0)
    return full, words


def _synthesize(text: str, voice: str, speed: float) -> np.ndarray:
    return _generate(text, voice, speed)[0]


def _wav_bytes(audio: np.ndarray) -> bytes:
    buf = io.BytesIO()
    sf.write(buf, audio, SAMPLE_RATE, format="WAV", subtype="PCM_16")
    return buf.getvalue()


# LRU cache of synthesized sentences: (text, voice, speed) -> response payload.
_CACHE_MAX = 256
_cache: OrderedDict = OrderedDict()
_cache_lock = threading.Lock()


def _cache_get(key):
    with _cache_lock:
        if key in _cache:
            _cache.move_to_end(key)
            return _cache[key]
    return None


def _cache_put(key, value):
    with _cache_lock:
        _cache[key] = value
        _cache.move_to_end(key)
        while len(_cache) > _CACHE_MAX:
            _cache.popitem(last=False)


def _warmup() -> None:
    try:
        with _lock:
            _synthesize("Ready.", DEFAULT_VOICE, 1.0)
        log.info("%s ready on %s.", ENGINE_NAME, DEVICE)
    except Exception:
        log.exception("Warmup failed — first request will retry.")
        return
    # Prefetch every voice so later switches work instantly (and offline).
    ok = 0
    for vid in VOICES:
        try:
            lang, resolved = _resolve_voice(vid)
            if isinstance(resolved, str):
                _get_pipeline(lang).load_voice(resolved)
            ok += 1
        except Exception as exc:
            log.warning("Could not prefetch voice %s: %s", vid, exc)
    log.info("Prefetched %d/%d voices.", ok, len(VOICES))


app = FastAPI(title="Minimal TTS")


class SpeakRequest(BaseModel):
    text: str = Field(min_length=1, max_length=MAX_CHARS)
    voice: str = DEFAULT_VOICE
    speed: float = Field(default=1.0, ge=0.5, le=2.0)
    format: str = Field(default="wav", pattern="^(wav|opus)$")


@app.get("/")
def index():
    # no-cache = revalidate on each load, so UI updates land on plain refresh
    return FileResponse(PROJECT_DIR / "index.html", headers={"Cache-Control": "no-cache"})


@app.get("/api/voices")
def voices():
    return {
        "voices": [
            {"id": vid, "label": label, "group": group}
            for vid, (label, group) in VOICES.items()
        ],
        "default": DEFAULT_VOICE,
        "engine": f"{ENGINE_NAME} · {DEVICE}",
        "device": DEVICE,
    }


def _validated(req) -> str:
    text = req.text.strip()
    if not text:
        raise HTTPException(status_code=400, detail="Nothing to read.")
    if req.voice not in VOICES:
        raise HTTPException(status_code=400, detail=f"Unknown voice {req.voice!r}.")
    return text


def _generate_locked(text: str, voice: str, speed: float, with_words: bool):
    if not _lock.acquire(timeout=0.5):
        if _model is None:
            raise HTTPException(
                status_code=503,
                detail="The model is still loading — try again in a few seconds.",
            )
        _lock.acquire()  # just waiting behind an in-flight generation
    try:
        try:
            return _generate(text, voice, speed, with_words)
        except ValueError:
            raise HTTPException(status_code=400, detail="No speakable content in the text.")
        except Exception:
            log.exception("Synthesis failed")
            raise HTTPException(status_code=500, detail="Synthesis failed — see server log.")
    finally:
        _lock.release()


@app.post("/api/tts")
def tts(req: SpeakRequest):
    text = _validated(req)
    audio, _ = _generate_locked(text, req.voice, req.speed, with_words=False)
    if req.format == "opus":
        buf = io.BytesIO()
        sf.write(buf, audio, SAMPLE_RATE, format="OGG", subtype="OPUS")
        return Response(content=buf.getvalue(), media_type="audio/ogg")
    return Response(content=_wav_bytes(audio), media_type="audio/wav")


class SentenceRequest(BaseModel):
    text: str = Field(min_length=1, max_length=5_000)
    voice: str = DEFAULT_VOICE
    speed: float = Field(default=1.0, ge=0.5, le=2.0)


@app.post("/api/sentence")
def sentence(req: SentenceRequest):
    """One sentence -> base64 WAV + word timings. Cached for instant replays."""
    text = _validated(req)
    key = (text, req.voice, round(req.speed, 2))
    hit = _cache_get(key)
    if hit is not None:
        return hit
    audio, words = _generate_locked(text, req.voice, req.speed, with_words=True)
    payload = {
        "audio": base64.b64encode(_wav_bytes(audio)).decode(),
        "duration": round(len(audio) / SAMPLE_RATE, 3),
        "words": words,
    }
    _cache_put(key, payload)
    return payload


def main() -> None:
    threading.Thread(target=_warmup, daemon=True).start()
    log.info("Serving on http://%s:%d", HOST, PORT)
    if not os.environ.get("MINIMAL_TTS_NO_BROWSER"):
        timer = threading.Timer(1.5, webbrowser.open, args=(f"http://{HOST}:{PORT}",))
        timer.daemon = True
        timer.start()
    uvicorn.run(app, host=HOST, port=PORT, log_level="warning")


if __name__ == "__main__":
    main()
