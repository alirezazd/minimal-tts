"""Minimal TTS — a local read-aloud server with a single-page UI.

Run:  uv run main.py   (opens http://127.0.0.1:8765 in your browser)
"""

import base64
import io
import logging
import os
import platform
import shutil
import subprocess
import threading
import time
import urllib.request
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
GAIN = 1.5
MAX_CHARS = 20_000

# The first letter of the id selects the language pipeline: a=US, b=UK.
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
# Weighted blends of preset voice tensors — more am_onyx = deeper.
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

# App mode tracks open windows by heartbeat so it can quit once the last closes.
_clients: dict = {}
_clients_lock = threading.Lock()
_ever_connected = False
CLIENT_STALE = 100.0   # forget a window we haven't heard from in this long (crash backstop)
CLOSE_GRACE = 4.0      # wait after the last window leaves, so a reload doesn't kill us
STARTUP_GRACE = 60.0   # give up if no window ever connects


def _get_pipeline(lang_code: str):
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
    if voice in CUSTOM_VOICES:
        recipe = CUSTOM_VOICES[voice]["recipe"]
        lang = recipe[0][0][0]
        if voice not in _blends:
            pipe = _get_pipeline(lang)
            _blends[voice] = sum(w * pipe.load_voice(base) for base, w in recipe)
        return lang, _blends[voice]
    return voice[0], voice


def _generate(text: str, voice: str, speed: float, with_words: bool = False):
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
    # Prefetch every voice so switches are instant and work offline.
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
    # revalidate each load so UI changes show on a plain refresh
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


@app.post("/api/ping")
def ping(id: str = ""):
    global _ever_connected
    with _clients_lock:
        _clients[id] = time.monotonic()
        _ever_connected = True
    return Response(status_code=204)


@app.post("/api/bye")
def bye(id: str = ""):
    with _clients_lock:
        _clients.pop(id, None)
    return Response(status_code=204)


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


# Chromeless Chrome "--app" window; fall back to a normal browser tab.
_CHROME_NAMES = ("google-chrome", "google-chrome-stable", "chromium", "chromium-browser",
                 "brave-browser", "microsoft-edge")


def _find_chrome():
    for name in _CHROME_NAMES:
        path = shutil.which(name)
        if path:
            return path
    candidates = []
    if platform.system() == "Windows":
        for env in ("PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"):
            base = os.environ.get(env)
            if base:
                candidates += [
                    Path(base) / "Google/Chrome/Application/chrome.exe",
                    Path(base) / "Microsoft/Edge/Application/msedge.exe",
                    Path(base) / "BraveSoftware/Brave-Browser/Application/brave.exe",
                ]
    elif platform.system() == "Darwin":
        candidates += [
            Path("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            Path("/Applications/Chromium.app/Contents/MacOS/Chromium"),
            Path("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
            Path("/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
        ]
    return next((str(c) for c in candidates if c.exists()), None)


def _open_window(url: str) -> None:
    chrome = _find_chrome()
    if not chrome:
        webbrowser.open(url)
        return
    try:
        subprocess.Popen(
            [chrome, f"--app={url}", f"--user-data-dir={PROJECT_DIR / '.chrome-profile'}",
             "--class=minimal-tts", "--no-first-run", "--no-default-browser-check",
             "--password-store=basic"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
    except OSError:
        webbrowser.open(url)


# Quit once the last window's heartbeat stops.
def _idle_watchdog(server) -> None:
    start = time.monotonic()
    empty_since = None
    while not server.should_exit:
        time.sleep(2.0)
        now = time.monotonic()
        with _clients_lock:
            for cid in [c for c, seen in _clients.items() if now - seen > CLIENT_STALE]:
                del _clients[cid]
            active, ever = bool(_clients), _ever_connected
        empty_since = None if active else (empty_since or now)
        if ever and not active and now - empty_since > CLOSE_GRACE:
            break
        if not ever and now - start > STARTUP_GRACE:
            break
    server.should_exit = True


def _server_already_running(url: str) -> bool:
    try:
        with urllib.request.urlopen(f"{url}/api/voices", timeout=0.5) as resp:
            return resp.status == 200
    except Exception:
        return False


def main() -> None:
    url = f"http://{HOST}:{PORT}"
    app_mode = not os.environ.get("MINIMAL_TTS_NO_BROWSER")
    if app_mode and _server_already_running(url):
        _open_window(url)   # another instance already owns the server; just add a window
        return
    threading.Thread(target=_warmup, daemon=True).start()
    server = uvicorn.Server(uvicorn.Config(app, host=HOST, port=PORT, log_level="warning"))
    log.info("Serving on %s", url)
    if app_mode:
        timer = threading.Timer(1.5, _open_window, args=(url,))
        timer.daemon = True
        timer.start()
        threading.Thread(target=_idle_watchdog, args=(server,), daemon=True).start()
    server.run()


if __name__ == "__main__":
    main()
