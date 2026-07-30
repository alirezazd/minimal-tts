//! Playback engine: gapless sentence queue on a cpal output stream.
//!
//! The audio callback is the clock: it advances a packed atomic
//! (sentence index << 32 | source sample position) that the UI reads
//! lock-free to drive word highlighting — the native equivalent of
//! Web Audio's currentTime.

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub const SRC_RATE: f64 = 24_000.0;
// no makeup gain: any >1x boost only clips against the [-1, 1] clamp below
const IDLE: u64 = u64::MAX;

#[derive(Clone)]
pub struct Segment {
    pub sentence: u32,
    /// start offset in source samples (used when seeking into a sentence)
    pub offset: usize,
    pub data: Arc<Vec<f32>>,
}

struct State {
    cur: Option<Segment>,
    /// fractional source-sample cursor within `cur`
    pos: f64,
    queue: VecDeque<Segment>,
}

pub struct Audio {
    _stream: cpal::Stream,
    state: Arc<Mutex<State>>,
    paused: Arc<AtomicBool>,
    epoch: Arc<AtomicU64>,
    clock: Arc<AtomicU64>,
}

impl Audio {
    pub fn new() -> Result<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device().context("no audio output device")?;
        let config = device.default_output_config()?;
        let sample_format = config.sample_format();
        let config: cpal::StreamConfig = config.into();
        let channels = config.channels as usize;
        let step = SRC_RATE / config.sample_rate.0 as f64;

        let state = Arc::new(Mutex::new(State { cur: None, pos: 0.0, queue: VecDeque::new() }));
        let paused = Arc::new(AtomicBool::new(false));
        let clock = Arc::new(AtomicU64::new(IDLE));

        let st = state.clone();
        let pa = paused.clone();
        let ck = clock.clone();
        let fill = move |out: &mut [f32]| {
            if pa.load(Ordering::Relaxed) {
                out.fill(0.0);
                return;
            }
            // never block the real-time callback: emit one silent buffer on the
            // rare contention with a control-thread mutation. A poisoned lock
            // (control thread panicked mid-mutation) is recovered rather than
            // silenced forever — the audio state is still coherent enough to read.
            let mut s = match st.try_lock() {
                Ok(guard) => guard,
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    out.fill(0.0);
                    return;
                }
            };
            for frame in out.chunks_mut(channels) {
                let mut v = 0.0f32;
                loop {
                    let advance = match &s.cur {
                        Some(seg) => {
                            let data = &seg.data;
                            let i = s.pos as usize;
                            if i + 1 < data.len() {
                                let frac = (s.pos - i as f64) as f32;
                                v = data[i] * (1.0 - frac) + data[i + 1] * frac;
                                ck.store(
                                    ((seg.sentence as u64) << 32) | i as u64,
                                    Ordering::Relaxed,
                                );
                                s.pos += step;
                                false
                            } else {
                                true // segment exhausted
                            }
                        }
                        None => {
                            ck.store(IDLE, Ordering::Relaxed);
                            break;
                        }
                    };
                    if !advance {
                        break;
                    }
                    s.cur = s.queue.pop_front();
                    s.pos = s.cur.as_ref().map_or(0.0, |seg| seg.offset as f64);
                }
                let v = v.clamp(-1.0, 1.0);
                frame.fill(v);
            }
        };

        let err = |e| eprintln!("audio stream error: {e}");
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config,
                move |out: &mut [f32], _| fill(out),
                err,
                None,
            )?,
            cpal::SampleFormat::I16 => {
                let mut buf = Vec::new();
                device.build_output_stream(
                    &config,
                    move |out: &mut [i16], _| {
                        buf.resize(out.len(), 0.0);
                        fill(&mut buf);
                        for (o, s) in out.iter_mut().zip(&buf) {
                            *o = (s * 32767.0) as i16;
                        }
                    },
                    err,
                    None,
                )?
            }
            f => anyhow::bail!("unsupported sample format {f}"),
        };
        stream.play()?;
        Ok(Self {
            _stream: stream,
            state,
            paused,
            epoch: Arc::new(AtomicU64::new(0)),
            clock,
        })
    }

    /// Invalidate everything queued and start playing `seg` immediately.
    /// Returns the new epoch; later `enqueue` calls must present it.
    pub fn play_now(&self, seg: Segment) -> u64 {
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let mut s = self.state.lock().unwrap();
        s.queue.clear();
        s.pos = seg.offset as f64;
        self.clock
            .store(((seg.sentence as u64) << 32) | seg.offset as u64, Ordering::Relaxed);
        s.cur = Some(seg);
        epoch
    }

    /// Append a segment for gapless continuation (ignored if stale).
    pub fn enqueue(&self, epoch: u64, seg: Segment) {
        if self.epoch.load(Ordering::SeqCst) != epoch {
            return;
        }
        self.state.lock().unwrap().queue.push_back(seg);
    }

    /// Drop queued segments but keep the currently playing one.
    pub fn clear_queue(&self) {
        self.state.lock().unwrap().queue.clear();
    }

    pub fn stop(&self) -> u64 {
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let mut s = self.state.lock().unwrap();
        s.cur = None;
        s.queue.clear();
        self.clock.store(IDLE, Ordering::Relaxed);
        self.paused.store(false, Ordering::Relaxed);
        epoch
    }

    pub fn set_paused(&self, p: bool) {
        self.paused.store(p, Ordering::Relaxed);
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// (sentence index, seconds into that sentence) — None when idle.
    pub fn position(&self) -> Option<(u32, f64)> {
        let c = self.clock.load(Ordering::Relaxed);
        if c == IDLE {
            return None;
        }
        Some(((c >> 32) as u32, (c & 0xffff_ffff) as f64 / SRC_RATE))
    }
}
