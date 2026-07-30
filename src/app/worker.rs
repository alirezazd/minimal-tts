//! The synthesis worker thread and the sentence audio cache.

use crate::synth::{Synthesizer, WordTiming};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};

pub(crate) const CACHE_CAP: usize = 384; // a full 25k-char doc (~300 sentences) stays resident
// ---------------------------------------------------------------- worker

pub(crate) type Key = (String, u32, String); // (voice, speed*100, sentence text)

pub(crate) struct SentAudio {
    pub(crate) samples: Arc<Vec<f32>>,
    pub(crate) timings: Vec<WordTiming>,
}

pub(crate) struct Req {
    pub(crate) epoch: u64,
    pub(crate) sent: u32,
    pub(crate) key: Key,
    pub(crate) keep: bool, // export requests bypass the epoch guard so they can't be dropped
}

pub(crate) struct Resp {
    pub(crate) sent: u32,
    pub(crate) key: Key,
    pub(crate) result: Result<SentAudio, String>,
}

pub(crate) fn spawn_worker(
    mut synth: Synthesizer,
    rx: mpsc::Receiver<Req>,
    tx: mpsc::Sender<Resp>,
    live_epoch: Arc<AtomicU64>,
) {
    std::thread::spawn(move || {
        while let Ok(req) = rx.recv() {
            if !req.keep && req.epoch < live_epoch.load(Ordering::SeqCst) {
                continue; // superseded before it started
            }
            let (voice, speed100, text) = &req.key;
            let result = synth
                .synthesize(text, voice, *speed100 as f32 / 100.0)
                .map(|(samples, timings)| SentAudio { samples: Arc::new(samples), timings })
                .map_err(|e| e.to_string());
            if tx.send(Resp { sent: req.sent, key: req.key, result }).is_err() {
                break;
            }
        }
    });
}

// ---------------------------------------------------------------- app state

pub(crate) struct Lru {
    pub(crate) map: HashMap<Key, Rc<SentAudio>>,
    pub(crate) order: VecDeque<Key>,
}

impl Lru {
    pub(crate) fn get(&mut self, k: &Key) -> Option<Rc<SentAudio>> {
        let hit = self.map.get(k).cloned();
        if hit.is_some() {
            self.order.retain(|o| o != k);
            self.order.push_back(k.clone());
        }
        hit
    }
    pub(crate) fn put(&mut self, k: Key, v: Rc<SentAudio>) {
        self.map.insert(k.clone(), v);
        self.order.retain(|o| o != &k);
        self.order.push_back(k);
        while self.order.len() > CACHE_CAP {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }
}
