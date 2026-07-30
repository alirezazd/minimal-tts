//! Rendering the whole document to a WAV file, off the playback path.

use super::{prepared_text, Key, Req, SentAudio};
use crate::synth::sentence_ranges;
use crate::MainWindow;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub(crate) struct Export {
    pub(crate) keys: Vec<Key>,                    // sentence order, for writing the WAV
    pub(crate) key_set: HashSet<Key>,             // O(1) membership on the hot synth-result path
    pub(crate) done: HashMap<Key, Rc<SentAudio>>, // private collection, immune to LRU eviction
    pub(crate) path: std::path::PathBuf,
}
/// UTC Y-M-D_H-M-S with no subprocess (civil-from-days; cross-platform).
pub(crate) fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let (days, tod) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}_{h:02}-{mi:02}-{s:02}")
}
pub(crate) fn downloads_dir() -> std::path::PathBuf {
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
    if let Ok(dirs) = std::fs::read_to_string(home.join(".config/user-dirs.dirs")) {
        for line in dirs.lines() {
            if let Some(v) = line.strip_prefix("XDG_DOWNLOAD_DIR=") {
                let p = v.trim_matches('"').replace("$HOME", &home.to_string_lossy());
                return std::path::PathBuf::from(p);
            }
        }
    }
    home.join("Downloads")
}

impl super::App {
    pub(crate) fn start_export(&mut self, ui: &MainWindow) {
        if self.export.is_some() {
            return;
        }
        let text = prepared_text(ui); // exactly what the editor shows
        let sents = sentence_ranges(&text);
        if sents.is_empty() {
            ui.set_status("Nothing to export.".into());
            return;
        }
        let speed100 = self.speed100();
        let keys: Vec<Key> = sents
            .iter()
            .map(|&(a, b)| (self.voice.clone(), speed100, text[a..b].to_string()))
            .collect();
        // seed already-synthesized sentences from the cache; pump_export pulls the
        // rest in one at a time so it can never starve live playback on the channel
        let mut done: HashMap<Key, Rc<SentAudio>> = HashMap::new();
        for key in &keys {
            if let Some(entry) = self.cache.get(key) {
                done.insert(key.clone(), entry);
            }
        }
        let key_set: HashSet<Key> = keys.iter().cloned().collect();
        let path = downloads_dir().join(format!("tts-{}.wav", timestamp()));
        self.export = Some(Export { keys, key_set, done, path });
    }

    /// Feed the worker one export synthesis at a time so a reader request never
    /// waits behind more than a single export job on the shared channel.
    pub(crate) fn pump_export(&mut self) {
        let next = {
            let Some(export) = self.export.as_ref() else { return };
            if export.keys.iter().any(|k| self.requested.contains(k)) {
                return; // one export job already in flight
            }
            export
                .keys
                .iter()
                .find(|k| !export.done.contains_key(*k) && !self.unspeakable.contains(*k))
                .cloned()
        };
        if let Some(key) = next {
            self.requested.insert(key.clone());
            let _ = self.tx.send(Req {
                epoch: self.worker_epoch.load(Ordering::SeqCst),
                sent: u32::MAX, // sentinel: never matches a reader's pending sentence
                key,
                keep: true, // entering read or changing voice can't drop it
            });
        }
    }

    pub(crate) fn poll_export(&mut self, ui: &MainWindow) {
        let Some(export) = self.export.as_ref() else { return };
        let unique: HashSet<&Key> = export.keys.iter().collect();
        // a sentence is resolved once it's synthesized or known unspeakable
        let resolved = unique
            .iter()
            .filter(|k| export.done.contains_key(**k) || self.unspeakable.contains(**k))
            .count();
        if resolved < unique.len() {
            let have = unique.iter().filter(|k| export.done.contains_key(**k)).count();
            ui.set_export_progress((resolved as f32 / unique.len().max(1) as f32).min(0.99));
            ui.set_status(format!("Exporting… {have}/{}", unique.len()).into());
            return;
        }
        let export = self.export.take().unwrap();
        if export.done.is_empty() {
            // every sentence was unspeakable — don't write a header-only WAV
            ui.set_status("Nothing speakable to export.".into());
            self.status_until = Some(Instant::now() + Duration::from_secs(6));
            return;
        }
        let write = || -> Result<()> {
            let mut w = hound::WavWriter::create(&export.path, crate::synth::wav_spec())?;
            for key in &export.keys {
                if let Some(entry) = export.done.get(key) {
                    for s in entry.samples.iter() {
                        w.write_sample(crate::synth::pcm16(*s))?;
                    }
                }
            }
            w.finalize()?;
            Ok(())
        };
        match write() {
            Ok(()) => ui.set_status(format!("Saved {}", export.path.display()).into()),
            Err(e) => ui.set_status(format!("Export failed: {e}").into()),
        }
        self.status_until = Some(Instant::now() + Duration::from_secs(8));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

        #[test]
    fn timestamp_shape() {
        let t = timestamp();
        assert_eq!(t.len(), 19, "YYYY-MM-DD_HH-MM-SS");
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[10..11], "_");
        assert!(t[..4].parse::<u32>().unwrap() >= 2026);
    }
}
