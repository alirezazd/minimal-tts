//! GUI orchestrator: document layout, synthesis pipeline, playback control,
//! and the 60 fps tick that drives highlights, scroll, and the aurora.

use crate::audio::{Audio, Segment};
use crate::synth::{Synthesizer, DEFAULT_VOICE, SAMPLE_RATE, VOICE_GROUPS};
use crate::{MainWindow, SearchBox};
use anyhow::Result;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

mod editor;
mod export;
mod layout;
mod media;
mod search;
mod worker;

use editor::{prepared_text, redo, sanitize};
use export::Export;
use layout::{build_doc, doc_first_word, install_app_font, make_blob, Doc, APP_FONT_FAMILY,
             BLOB_STOPS, DROP_STOPS, PAD_X, PAD_Y};
use worker::{spawn_worker, Key, Lru, Req, Resp, SentAudio};

const MAX_CHARS: usize = 25_000;


fn voice_label(id: &str, group: &str) -> String {
    let name = id.split_once('_').map_or(id, |(_, n)| n);
    let mut c = name.chars();
    let cap: String = c.next().map(|f| f.to_uppercase().collect::<String>()).unwrap_or_default() + c.as_str();
    if group == "Custom" {
        cap
    } else {
        format!("{cap} · {group}")
    }
}



struct App {
    ui: slint::Weak<MainWindow>,
    engine: Audio,
    epoch: u64,
    worker_epoch: Arc<AtomicU64>,
    tx: mpsc::Sender<Req>,
    rx: mpsc::Receiver<Resp>,
    cache: Lru,
    requested: HashSet<Key>,
    unspeakable: HashSet<Key>, // negative cache: sentences g2p reports as empty
    doc: Option<Doc>,
    voice_ids: Vec<String>,
    voice: String,
    speed: f32,
    speed_pending: Option<(f32, Instant)>,
    reading: bool,
    was_playing: bool,
    want_pause: bool, // user paused (possibly while audio was still loading)
    cur_sent: u32,
    enq_next: u32,
    pending: Option<(u32, Option<u32>)>, // sentence waiting on synth (+ word to seek)
    live: HashMap<u32, Rc<SentAudio>>,
    fade: Option<(i32, Instant)>,
    hl: (f32, f32, f32, f32, f32), // x y w h opacity
    hl_target: Option<(f32, f32, f32, f32)>,
    last_word_ui: i32,
    scroll_target: Option<f32>,
    user_scroll_until: Instant,
    last_scroll_set: f32,
    energy: f32,
    spin: f32, // spinner angle while synthesizing / exporting
    aur_mix: f32, // 0 = idle drift, 1 = full ink vortex; follows energy slowly
    aur_swirl: f32,
    aur_time: f32,
    last_tick: Instant,
    last_saved_text: String,
    save_due: Option<Instant>,
    banner_until: Option<Instant>, // auto-tidy banner auto-dismiss
    editor_len: usize,             // last seen editor length, to spot a paste
    pending_tidy: bool,            // paste looked messy; tidy it on the next tick
    tidy_source: Option<String>,   // pre-tidy text, so undoing a tidy isn't re-tidied
    search_query: String,
    search_hits: Vec<(usize, usize)>, // byte ranges into the searched text
    search_idx: usize,
    status_until: Option<Instant>,
    resume_hash: u64,
    resume_idx: u32,
    export: Option<Export>,
    media: Option<souvlaki::MediaControls>,
    media_rx: Option<mpsc::Receiver<souvlaki::MediaControlEvent>>,
    tick_n: u64,
}




fn text_hash(s: &str) -> u64 {
    let mut h: u64 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33) ^ b as u64;
    }
    h
}



fn config_path() -> std::path::PathBuf {
    crate::synth::config_dir().join("state.json")
}

impl App {
    fn speed100(&self) -> u32 {
        (self.speed * 100.0).round() as u32
    }

    fn key_for(&self, sent: u32) -> Option<Key> {
        let doc = self.doc.as_ref()?;
        let text = &doc.sentences.get(sent as usize)?.text;
        Some((self.voice.clone(), self.speed100(), text.clone()))
    }

    fn request(&mut self, sent: u32) {
        if let Some(key) = self.key_for(sent) {
            if self.cache.map.contains_key(&key)
                || self.requested.contains(&key)
                || self.unspeakable.contains(&key)
            {
                return;
            }
            self.requested.insert(key.clone());
            let _ = self.tx.send(Req {
                epoch: self.worker_epoch.load(Ordering::SeqCst),
                sent,
                key,
                keep: false,
            });
        }
    }

    fn segment(&self, sent: u32, entry: &Rc<SentAudio>, offset: usize) -> Segment {
        Segment { sentence: sent, offset, data: entry.samples.clone() }
    }

    fn mark_sentence(&mut self, ui: &MainWindow, sent: i32) {
        let prev = ui.get_current_sent();
        if prev == sent {
            return;
        }
        if prev >= 0 {
            ui.set_fade_sent(prev);
            ui.set_fade_level(1.0);
            self.fade = Some((prev, Instant::now()));
        }
        ui.set_current_sent(sent);
        ui.set_current_word(-1);
        self.last_word_ui = -1;
        self.hl_target = None;
        self.hl.4 = 0.0;
        ui.set_hl_opacity(0.0);
    }

    fn play_from(&mut self, ui: &MainWindow, sent: u32, word: Option<u32>) {
        let Some((n_sentences, first_word)) = self
            .doc
            .as_ref()
            .map(|d| (d.sentences.len(), doc_first_word(d, sent)))
        else {
            return;
        };
        if sent as usize >= n_sentences {
            self.enter_edit(ui);
            return;
        }
        self.mark_sentence(ui, sent as i32);
        self.cur_sent = sent;
        self.scroll_to_word(first_word, true);
        let key = self.key_for(sent).unwrap();
        if let Some(entry) = self.cache.get(&key) {
            // clamp inside the sentence: predicted word starts can slightly
            // exceed the real audio length, and an out-of-range seek would
            // immediately exhaust the segment (kicking us back to edit mode)
            let dur = entry.samples.len() as f32 / SAMPLE_RATE as f32;
            self.note_dur(sent, &key, &entry); // enq_next starts past this one
            let offset_secs = word
                .and_then(|k| entry.timings.get(k as usize))
                .map(|t| (t.start - 0.02).max(0.0))
                .unwrap_or(0.0)
                .min((dur - 0.05).max(0.0));
            self.epoch = self
                .engine
                .play_now(self.segment(sent, &entry, (offset_secs * SAMPLE_RATE as f32) as usize));
            // honor a pause the user issued while this sentence was still loading
            self.engine.set_paused(self.want_pause);
            self.live.clear();
            self.live.insert(sent, entry);
            self.enq_next = sent + 1;
            self.pending = None;
            self.was_playing = true;
            ui.set_playing(!self.want_pause);
            ui.set_status("".into());
            self.media_playback(!self.want_pause);
        } else {
            self.epoch = self.engine.stop(); // stop() also clears the paused flag
            self.pending = Some((sent, word));
            self.request(sent);
            ui.set_playing(!self.want_pause); // optimistic; audio starts when synthesis lands
        }
        self.request(sent + 1);
        self.request(sent + 2);
    }

    fn enter_read(&mut self, ui: &MainWindow) {
        let text = prepared_text(ui);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            ui.set_status("Nothing to read.".into());
            return;
        }
        if trimmed.len() > MAX_CHARS {
            ui.set_status(format!("Text is {} characters — the limit is {MAX_CHARS}.", trimmed.len()).into());
            return;
        }
        let width = ui.get_reader_width() - 2.0 * PAD_X;
        if width < 60.0 {
            return;
        }
        // lay out the prepared (tidied) text — the reader shows this, not the editor
        let doc = build_doc(&text, width, ui.window().scale_factor());
        if doc.sentences.is_empty() {
            ui.set_status("Nothing to read.".into());
            return;
        }
        ui.set_words(ModelRc::from(Rc::new(VecModel::from(doc.ui_words.clone()))));
        ui.set_doc_height(doc.height);
        ui.set_reading(true);
        ui.set_status("".into());
        ui.set_current_sent(-1);
        ui.set_current_word(-1);
        ui.set_reader_scroll(0.0);
        self.last_scroll_set = 0.0;
        let n_sent = doc.sentences.len() as u32;
        if let Some(m) = self.media.as_mut() {
            let title: String = trimmed.chars().take(80).collect();
            let _ = m.set_metadata(souvlaki::MediaMetadata {
                title: Some(&title),
                artist: Some("Minimal TTS"),
                ..Default::default()
            });
        }
        self.doc = Some(doc);
        self.reading = true;
        if ui.get_search_open() {
            self.run_search(ui, false); // re-map open search onto the reader text
        }
        ui.invoke_focus_keys();
        self.bump_epoch();
        // resume where this exact text was left off
        let start = if text_hash(text.trim()) == self.resume_hash {
            self.resume_idx.min(n_sent - 1)
        } else {
            0
        };
        self.want_pause = false;
        self.play_from(ui, start, None);
    }

    fn enter_edit(&mut self, ui: &MainWindow) {
        self.bump_epoch();
        self.engine.stop();
        self.reading = false;
        self.was_playing = false;
        self.pending = None;
        self.live.clear();
        ui.set_reading(false);
        ui.set_playing(false);
        ui.set_current_sent(-1);
        ui.set_current_word(-1);
        ui.set_progress(0.0);
        ui.set_time_label("".into());
        ui.invoke_focus_editor();
        self.media_playback(false);
        if ui.get_search_open() {
            self.run_search(ui, false); // re-map open search onto the editor text
        }
    }

    fn bump_epoch(&mut self) {
        self.worker_epoch.fetch_add(1, Ordering::SeqCst);
        self.requested.clear();
    }

    /// Memoize a sentence's real duration the moment its audio is in hand, so
    /// update_progress never rebuilds cache keys on the tick path. The key is
    /// re-checked against the live doc because audio can outlive the request
    /// that asked for it: a stale-voice result, or one for a document the user
    /// has since edited. Export jobs carry sent = u32::MAX and fall out here.
    fn note_dur(&mut self, sent: u32, key: &Key, entry: &SentAudio) {
        if key.0 != self.voice || key.1 != self.speed100() {
            return;
        }
        let dur = entry.samples.len() as f32 / SAMPLE_RATE as f32;
        if let Some(s) = self.doc.as_mut().and_then(|d| d.sentences.get_mut(sent as usize)) {
            if s.text == key.2 {
                s.dur = Some(dur);
            }
        }
    }

    /// Drop queued-but-unplayed sentences (voice/speed changed midway).
    fn refresh_ahead(&mut self) {
        // the memoized durations describe the old voice/speed
        if let Some(doc) = self.doc.as_mut() {
            doc.sentences.iter_mut().for_each(|s| s.dur = None);
        }
        if !self.reading {
            return;
        }
        self.bump_epoch();
        self.epoch = self.engine.epoch(); // stale enqueues die with the old epoch
        self.engine.clear_queue();
        self.live.retain(|&s, _| s == self.cur_sent);
        // if still waiting on the current sentence, re-request it under the new
        // voice/speed — bump_epoch just dropped the old in-flight request
        self.enq_next = match self.pending {
            Some((sent, _)) => {
                self.request(sent);
                sent + 1
            }
            None => self.cur_sent + 1,
        };
        self.request(self.enq_next);
    }

    fn scroll_to_word(&mut self, idx: Option<usize>, force: bool) {
        let Some(doc) = self.doc.as_ref() else { return };
        let Some(i) = idx else { return };
        if let Some(w) = doc.ui_words.get(i) {
            if force || Instant::now() >= self.user_scroll_until {
                self.scroll_target = Some(w.y + PAD_Y);
            }
        }
    }

    fn tick(&mut self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f32().min(0.1);
        self.last_tick = now;
        self.tick_n += 1;

        if self.pending_tidy {
            self.pending_tidy = false;
            self.tidy_editor(&ui);
        }
        self.drain_media(&ui);

        // synth results
        while let Ok(resp) = self.rx.try_recv() {
            self.requested.remove(&resp.key);
            match resp.result {
                Ok(sa) => {
                    let entry = Rc::new(sa);
                    if let Some(export) = self.export.as_mut() {
                        if export.key_set.contains(&resp.key) {
                            export.done.insert(resp.key.clone(), entry.clone());
                        }
                    }
                    self.note_dur(resp.sent, &resp.key, &entry);
                    self.cache.put(resp.key, entry);
                }
                Err(e) if e.contains("no speakable content") => {
                    // negative-cache it: the reader's pending block and the export
                    // poller both treat unspeakable sentences as resolved, and
                    // request() never re-issues them (was a per-tick re-phonemize)
                    self.unspeakable.insert(resp.key.clone());
                }
                Err(e) => {
                    // hard synth failure: abort an export needing this key, and drop
                    // the reader to edit mode only if it was waiting on this sentence
                    // (export requests carry sent = u32::MAX, so they never match)
                    if self.export.as_ref().map_or(false, |x| x.key_set.contains(&resp.key)) {
                        self.export = None;
                        ui.set_status(format!("Export failed: {e}").into());
                        self.status_until = Some(Instant::now() + Duration::from_secs(8));
                    }
                    if self.reading && self.pending.map_or(false, |(s, _)| s == resp.sent) {
                        ui.set_status(e.into());
                        self.enter_edit(&ui);
                    }
                }
            }
        }

        if self.reading {
            // start pending playback once its audio lands
            if let Some((sent, word)) = self.pending {
                if let Some(key) = self.key_for(sent) {
                    if self.unspeakable.contains(&key) {
                        // web parity: an unspeakable sentence is skipped forward
                        self.pending = None;
                        self.play_from(&ui, sent + 1, None);
                    } else if self.cache.map.contains_key(&key) {
                        self.pending = None;
                        self.play_from(&ui, sent, word);
                    }
                }
            }

            // keep the queue topped up
            if self.pending.is_none() {
                let last = self.doc.as_ref().map_or(0, |d| d.sentences.len() as u32 - 1);
                while self.enq_next <= (self.cur_sent + 2).min(last) {
                    let Some(key) = self.key_for(self.enq_next) else { break };
                    if self.unspeakable.contains(&key) {
                        self.enq_next += 1; // skip an unspeakable sentence, don't enqueue
                        continue;
                    }
                    if let Some(entry) = self.cache.get(&key) {
                        self.engine.enqueue(self.epoch, self.segment(self.enq_next, &entry, 0));
                        self.note_dur(self.enq_next, &key, &entry); // cache hit: no Resp comes
                        self.live.insert(self.enq_next, entry);
                        self.enq_next += 1;
                    } else {
                        self.request(self.enq_next);
                        break;
                    }
                }
            }

            match self.engine.position() {
                Some((sent, t)) => {
                    if sent != self.cur_sent {
                        self.cur_sent = sent;
                        self.mark_sentence(&ui, sent as i32);
                        self.live.retain(|&s, _| s >= sent);
                        if sent > 0 {
                            self.resume_hash =
                                text_hash(ui.get_editor_text().to_string().trim());
                            self.resume_idx = sent;
                            self.save_due = Some(now + Duration::from_secs(1));
                        }
                    }
                    let mut new_word: Option<(i32, (f32, f32, f32, f32))> = None;
                    let mut rms_target = 0.0f32;
                    if let (Some(doc), Some(entry)) = (self.doc.as_ref(), self.live.get(&sent)) {
                        let tim = &entry.timings;
                        let mut k = tim.partition_point(|w| (w.start as f64) <= t);
                        if k > 0 {
                            k -= 1;
                            let info = &doc.sentences[sent as usize];
                            let cur_word =
                                (info.first_word + k.min(info.n_words.saturating_sub(1))) as i32;
                            if cur_word != self.last_word_ui {
                                let w = &doc.ui_words[cur_word as usize];
                                new_word = Some((
                                    cur_word,
                                    (w.x + PAD_X - 3.0, w.y + PAD_Y - 2.0, w.w + 6.0, w.h + 4.0),
                                ));
                            }
                        }
                        // energy from the samples around the playhead
                        let pos = (t * SAMPLE_RATE as f64) as usize;
                        let s = &entry.samples;
                        let a = pos.saturating_sub(512).min(s.len());
                        let b = (pos + 512).min(s.len());
                        if b > a {
                            let rms =
                                (s[a..b].iter().map(|v| v * v).sum::<f32>() / (b - a) as f32).sqrt();
                            rms_target = (rms * 8.0).min(1.0);
                        }
                    }
                    if let Some((cur_word, hl)) = new_word {
                        self.last_word_ui = cur_word;
                        ui.set_current_word(cur_word);
                        self.hl_target = Some(hl);
                        self.scroll_to_word(Some(cur_word as usize), false);
                    }
                    let k_e = if rms_target > self.energy { 0.06 } else { 0.010 } * (dt / 0.016);
                    self.energy += (rms_target - self.energy) * k_e.min(1.0);
                    if self.tick_n % 10 == 0 {
                        self.update_progress(&ui, t as f32);
                    }
                    self.was_playing = true;
                }
                None => {
                    let last = self.doc.as_ref().map_or(0, |d| d.sentences.len().saturating_sub(1) as u32);
                    if self.was_playing && self.pending.is_none() {
                        if self.cur_sent >= last {
                            self.resume_hash = 0; // finished — next read starts fresh
                            self.resume_idx = 0;
                            self.enter_edit(&ui); // natural end unlocks editing
                        } else {
                            // starved mid-stream: resume when the next sentence lands
                            self.pending = Some((self.cur_sent + 1, None));
                        }
                        self.was_playing = false;
                    }
                    let target = 0.0f32;
                    self.energy += (target - self.energy) * (0.008 * (dt / 0.016)).min(1.0);
                }
            }
        } else {
            self.energy += (0.0 - self.energy) * (0.008 * (dt / 0.016)).min(1.0);
        }

        // aurora physics: idle = independent slow orbits; speaking = the blobs
        // pour into a shared slow vortex, chasing and overlapping like ink.
        // The mix follows energy asymmetrically (~1s in, ~5s back to idle).
        const TAU: f32 = std::f32::consts::TAU;
        const BLOBS: [(f32, f32, f32, f32, f32, f32, f32); 4] = [
            // base_x base_y  amp_x amp_y  freq_x freq_y  phase
            (0.38, 0.30, 0.26, 0.22, 0.020, 0.016, 0.0),
            (0.64, 0.42, 0.24, 0.26, 0.014, 0.019, 1.9),
            (0.48, 0.66, 0.28, 0.20, 0.011, 0.017, 4.0),
            (0.52, 0.44, 0.18, 0.16, 0.023, 0.013, 2.7),
        ];
        self.aur_time += dt;
        let k_m = if self.energy > self.aur_mix { 0.030 } else { 0.005 } * (dt / 0.016);
        self.aur_mix += (self.energy - self.aur_mix) * k_m.min(1.0);
        self.aur_swirl += dt * TAU * (0.02 + 0.11 * self.aur_mix);
        if self.reading || self.tick_n % 2 == 0 {
            let (t, m) = (self.aur_time, self.aur_mix);
            let mut p = [(0.0f32, 0.0f32, 1.0f32); 4];
            for (i, b) in BLOBS.iter().enumerate() {
                let idle_x = b.0 + b.2 * (t * b.4 * TAU + b.6).sin();
                let idle_y = b.1 + b.3 * (t * b.5 * TAU + b.6 * 1.3).cos();
                let phi = i as f32 * TAU / 4.0;
                let r = 0.26 - 0.06 * (t * 0.10 * TAU + phi).sin() * m;
                let sx = 0.50 + r * (self.aur_swirl + phi).cos();
                let sy = 0.46 + r * 0.75 * (self.aur_swirl + phi).sin();
                let breathe = 0.05 + 0.10 * (0.5 + 0.5 * (t * 0.35 * TAU + phi).sin());
                p[i] = (idle_x + (sx - idle_x) * m, idle_y + (sy - idle_y) * m, 1.0 + m * breathe);
            }
            ui.set_b1x(p[0].0); ui.set_b1y(p[0].1); ui.set_b1s(p[0].2);
            ui.set_b2x(p[1].0); ui.set_b2y(p[1].1); ui.set_b2s(p[1].2);
            ui.set_b3x(p[2].0); ui.set_b3y(p[2].1); ui.set_b3s(p[2].2);
            ui.set_b4x(p[3].0); ui.set_b4y(p[3].1); ui.set_b4s(p[3].2);
            ui.set_energy(self.energy);
        }

        // highlight glide (snap on long jumps, like the web version)
        if let Some(t) = self.hl_target {
            let (x, y, w, h, o) = self.hl;
            if (t.1 - y).abs() > 80.0 || o < 0.05 {
                self.hl = (t.0, t.1, t.2, t.3, (o + 0.15).min(1.0));
            } else {
                let k = (14.0 * dt).min(1.0);
                self.hl = (x + (t.0 - x) * k, y + (t.1 - y) * k, w + (t.2 - w) * k, h + (t.3 - h) * k, (o + 0.15).min(1.0));
            }
            ui.set_hl_x(self.hl.0);
            ui.set_hl_y(self.hl.1);
            ui.set_hl_w(self.hl.2);
            ui.set_hl_h(self.hl.3);
            ui.set_hl_opacity(self.hl.4);
        }

        // previous sentence dims smoothly (0.9 s, ease-out) — hover stays instant
        if let Some((sent, start)) = self.fade {
            let x = (now - start).as_secs_f32() / 0.9;
            if x >= 1.0 {
                self.fade = None;
                ui.set_fade_sent(-1);
                ui.set_fade_level(0.0);
            } else {
                ui.set_fade_sent(sent);
                ui.set_fade_level((1.0 - x).powf(1.7));
            }
        }

        // auto-scroll with user override
        if self.reading {
            let vp = ui.get_reader_scroll();
            if (vp - self.last_scroll_set).abs() > 1.5 {
                self.user_scroll_until = now + Duration::from_secs(4);
                self.last_scroll_set = vp;
            }
            if let Some(target) = self.scroll_target {
                if now >= self.user_scroll_until {
                    let view_h = ui.get_reader_height();
                    let doc_h = ui.get_doc_height();
                    let want = -(target - view_h * 0.45).clamp(0.0, (doc_h - view_h).max(0.0));
                    let k = (6.0 * dt).min(1.0);
                    let new = vp + (want - vp) * k;
                    ui.set_reader_scroll(new);
                    self.last_scroll_set = new;
                }
            }
        }

        if self.banner_until.map_or(false, |t| now >= t) {
            self.banner_until = None;
            ui.set_tidy_banner(false);
        }
        if self.status_until.map_or(false, |t| now >= t) {
            self.status_until = None;
            ui.set_status("".into());
        }
        self.pump_export();
        // ~10 Hz is plenty for the resolved-sentence sweep; a no-op unless an
        // export is actually running
        if self.tick_n % 6 == 0 {
            self.poll_export(&ui);
        }

        // "working" spinner in the button we're waiting on
        let loading = self.pending.is_some();
        let exporting = self.export.is_some();
        if loading || exporting {
            self.spin = (self.spin + dt * 300.0) % 360.0;
            ui.set_spin(self.spin);
        }
        if ui.get_loading() != loading {
            ui.set_loading(loading);
        }
        if ui.get_exporting() != exporting {
            ui.set_exporting(exporting);
        }

        // debounced speed apply + state save
        if let Some((v, due)) = self.speed_pending {
            if now >= due {
                self.speed_pending = None;
                self.speed = v;
                self.refresh_ahead();
            }
        }
        if self.tick_n % 120 == 0 {
            let text = ui.get_editor_text().to_string();
            if text != self.last_saved_text {
                self.last_saved_text = text;
                self.save_due = Some(now);
            }
        }
        if self.save_due.map_or(false, |d| now >= d) {
            self.save_due = None;
            self.save_state(&ui);
        }
    }

    fn update_progress(&mut self, ui: &MainWindow, t_cur: f32) {
        let Some(doc) = self.doc.as_ref() else { return };
        // durations are memoized as audio lands (note_dur); un-synthesized
        // sentences are estimated from the seconds-per-char rate observed so far
        let mut known = 0.0f32;
        let mut known_chars = 0usize;
        for s in &doc.sentences {
            if let Some(d) = s.dur {
                known += d;
                known_chars += s.text.len();
            }
        }
        let rate = if known_chars > 0 { known / known_chars as f32 } else { 0.06 / self.speed };
        let mut elapsed = t_cur;
        let mut total = 0.0;
        let mut all_known = true;
        for (i, s) in doc.sentences.iter().enumerate() {
            let d = s.dur.unwrap_or_else(|| {
                all_known = false;
                s.text.len() as f32 * rate
            });
            total += d;
            if (i as u32) < self.cur_sent {
                elapsed += d;
            }
        }
        ui.set_progress(if total > 0.0 { (elapsed / total).min(1.0) } else { 0.0 });
        let fmt = |s: f32| format!("{}:{:02}", (s / 60.0) as u32, (s % 60.0) as u32);
        ui.set_time_label(
            format!("{} / {}{}", fmt(elapsed), if all_known { "" } else { "≈" }, fmt(total)).into(),
        );
    }

    fn save_state(&self, ui: &MainWindow) {
        let path = config_path();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let v = serde_json::json!({
            "text": ui.get_editor_text().as_str(),
            "voice": self.voice,
            "speed": self.speed,
            "resume_hash": self.resume_hash.to_string(),
            "resume_idx": self.resume_idx,
        });
        let _ = std::fs::write(path, v.to_string());
    }

    /// Resume/pause when there is (or will be) audio; no-op otherwise. Idempotent,
    /// so an OS media widget's explicit Play/Pause behaves as pressed.
    fn set_playing(&mut self, ui: &MainWindow, playing: bool) {
        if self.reading && (self.engine.position().is_some() || self.pending.is_some()) {
            // remember the intent so play_from honors it if audio is still loading
            self.want_pause = !playing;
            self.engine.set_paused(!playing);
            ui.set_playing(playing);
            self.media_playback(playing);
        }
    }

    fn toggle(&mut self, ui: &MainWindow) {
        if !self.reading {
            self.enter_read(ui);
        } else if self.engine.position().is_some() || self.pending.is_some() {
            let playing = self.engine.paused();
            self.set_playing(ui, playing);
        } else {
            let sent = self.cur_sent;
            self.want_pause = false;
            self.play_from(ui, sent, None);
        }
    }

    fn nav(&mut self, ui: &MainWindow, d: i32) {
        if !self.reading {
            return;
        }
        let last = self.doc.as_ref().map_or(0, |doc| doc.sentences.len().saturating_sub(1) as u32);
        let cur = self.cur_sent;
        let target = if d > 0 {
            (cur + 1).min(last)
        } else {
            let t = self.engine.position().map(|(_, t)| t).unwrap_or(0.0);
            if t > 2.0 || cur == 0 {
                cur
            } else {
                cur - 1
            }
        };
        self.want_pause = false;
        self.play_from(ui, target, None);
    }

}


// ---------------------------------------------------------------- entry

pub fn run() -> Result<()> {
    install_app_font();
    // skia renders large text far better than the default femtovg
    let _ = slint::BackendSelector::new().renderer_name("skia".into()).select();
    // Wayland app_id / X11 WM_CLASS — without it GNOME's dash shows "Unknown".
    // Must match the shipped minimal-tts.desktop (name + icon association).
    let _ = slint::set_xdg_app_id("minimal-tts");
    let g2p = crate::g2p::G2p::new()?;
    let synth = Synthesizer::new(g2p)?;
    let engine = Audio::new()?;

    let ui = MainWindow::new()?;
    ui.set_engine_label("Kokoro-82M".into());
    ui.set_ui_font(APP_FONT_FAMILY.into());
    ui.set_blob1(make_blob((0x81, 0x8c, 0xf8), 0.66, &BLOB_STOPS));
    ui.set_blob2(make_blob((0xc0, 0x84, 0xfc), 0.61, &BLOB_STOPS));
    ui.set_blob3(make_blob((0x38, 0xbd, 0xf8), 0.61, &BLOB_STOPS));
    ui.set_blob4(make_blob((0xa7, 0x8b, 0xfa), 0.69, &DROP_STOPS));

    // voices
    let mut voice_ids = Vec::new();
    let mut names: Vec<SharedString> = Vec::new();
    for (group, ids) in VOICE_GROUPS {
        for id in *ids {
            voice_ids.push(id.to_string());
            names.push(voice_label(id, group).into());
        }
    }
    ui.set_voice_names(ModelRc::from(Rc::new(VecModel::from(names))));

    // restore state
    let mut voice = DEFAULT_VOICE.to_string();
    let mut speed = 1.0f32;
    let mut resume_hash = 0u64;
    let mut resume_idx = 0u32;
    if let Ok(raw) = std::fs::read_to_string(config_path()) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(t) = v["text"].as_str() {
                // tidy restored text too, so the editor always shows exactly
                // what will be read. Safe to write directly here: the undo
                // stack doesn't exist until the window runs.
                let t = crate::tidy::tidy_if_messy(t).unwrap_or_else(|| t.into());
                ui.set_editor_text(sanitize(&t).into());
            }
            if let Some(vid) = v["voice"].as_str() {
                if voice_ids.iter().any(|i| i == vid) {
                    voice = vid.to_string();
                }
            }
            if let Some(s) = v["speed"].as_f64() {
                speed = (s as f32).clamp(0.5, 2.0);
            }
            resume_hash = v["resume_hash"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
            resume_idx = v["resume_idx"].as_u64().unwrap_or(0) as u32;
        }
    }
    ui.set_voice_index(voice_ids.iter().position(|i| i == &voice).unwrap_or(0) as i32);
    ui.set_speed(speed);
    ui.set_speed_label(format!("{speed:.2}×").into());

    let worker_epoch = Arc::new(AtomicU64::new(1));
    let (tx_req, rx_req) = mpsc::channel::<Req>();
    let (tx_resp, rx_resp) = mpsc::channel::<Resp>();
    spawn_worker(synth, rx_req, tx_resp, worker_epoch.clone());

    // OS media keys via MPRIS
    let (media, media_rx) = {
        let config = souvlaki::PlatformConfig {
            dbus_name: "minimal_tts",
            display_name: "Minimal TTS",
            hwnd: None,
        };
        match souvlaki::MediaControls::new(config) {
            Ok(mut controls) => {
                let (mtx, mrx) = mpsc::channel();
                match controls.attach(move |e| {
                    let _ = mtx.send(e);
                }) {
                    Ok(()) => (Some(controls), Some(mrx)),
                    Err(_) => (None, None),
                }
            }
            Err(_) => (None, None),
        }
    };

    let app = Rc::new(RefCell::new(App {
        ui: ui.as_weak(),
        engine,
        epoch: 0,
        worker_epoch,
        tx: tx_req,
        rx: rx_resp,
        cache: Lru { map: HashMap::new(), order: VecDeque::new() },
        requested: HashSet::new(),
        unspeakable: HashSet::new(),
        doc: None,
        voice_ids,
        voice,
        speed,
        speed_pending: None,
        reading: false,
        was_playing: false,
        want_pause: false,
        cur_sent: 0,
        enq_next: 0,
        pending: None,
        live: HashMap::new(),
        fade: None,
        hl: (0.0, 0.0, 0.0, 0.0, 0.0),
        hl_target: None,
        last_word_ui: -1,
        scroll_target: None,
        user_scroll_until: Instant::now(),
        last_scroll_set: 0.0,
        energy: 0.0,
        spin: 0.0,
        aur_mix: 0.0,
        aur_swirl: 0.0,
        aur_time: 0.0,
        last_tick: Instant::now(),
        last_saved_text: String::new(),
        save_due: None,
        banner_until: None,
        editor_len: ui.get_editor_text().len(),
        pending_tidy: false,
        tidy_source: None,
        search_query: String::new(),
        search_hits: Vec::new(),
        search_idx: 0,
        status_until: None,
        resume_hash,
        resume_idx,
        export: None,
        media,
        media_rx,
        tick_n: 0,
    }));

    {
        let a = app.clone();
        ui.on_start_reading(move || {
            let ui = a.borrow().ui.upgrade().unwrap();
            a.borrow_mut().enter_read(&ui);
        });
    }
    {
        let a = app.clone();
        ui.on_stop_reading(move || {
            let ui = a.borrow().ui.upgrade().unwrap();
            a.borrow_mut().enter_edit(&ui);
        });
    }
    {
        let a = app.clone();
        ui.on_toggle_play(move || {
            let ui = a.borrow().ui.upgrade().unwrap();
            a.borrow_mut().toggle(&ui);
        });
    }
    {
        let a = app.clone();
        ui.on_word_clicked(move |i| {
            let ui = a.borrow().ui.upgrade().unwrap();
            let mut app = a.borrow_mut();
            let hit = app.doc.as_ref().and_then(|doc| doc.word_of.get(i as usize).copied());
            if let Some((sent, k)) = hit {
                app.want_pause = false;
                app.play_from(&ui, sent, Some(k));
            }
        });
    }
    {
        let a = app.clone();
        ui.on_voice_changed(move |idx| {
            let ui = a.borrow().ui.upgrade().unwrap();
            let mut app = a.borrow_mut();
            if let Some(id) = app.voice_ids.get(idx as usize).cloned() {
                app.voice = id;
                app.refresh_ahead();
                app.save_state(&ui);
            }
        });
    }
    {
        let a = app.clone();
        ui.on_speed_changed(move |v| {
            let ui = a.borrow().ui.upgrade().unwrap();
            let mut app = a.borrow_mut();
            let snapped = (v * 20.0).round() / 20.0;
            ui.set_speed_label(format!("{snapped:.2}×").into());
            app.speed_pending = Some((snapped, Instant::now() + Duration::from_millis(250)));
        });
    }
    {
        let a = app.clone();
        ui.on_nav_sentence(move |d| {
            let ui = a.borrow().ui.upgrade().unwrap();
            a.borrow_mut().nav(&ui, d);
        });
    }
    {
        let a = app.clone();
        ui.on_search_changed(move |q| {
            let ui = a.borrow().ui.upgrade().unwrap();
            let mut app = a.borrow_mut();
            app.search_query = q.to_string();
            app.run_search(&ui, true);
        });
    }
    {
        let a = app.clone();
        ui.on_search_nav(move |d| {
            let ui = a.borrow().ui.upgrade().unwrap();
            a.borrow_mut().search_nav(&ui, d);
        });
    }
    {
        let a = app.clone();
        ui.on_search_closed(move || {
            let ui = a.borrow().ui.upgrade().unwrap();
            let mut app = a.borrow_mut();
            app.search_hits.clear();
            app.search_idx = 0;
            ui.set_search_count("".into());
            ui.set_search_boxes(ModelRc::from(Rc::new(VecModel::<SearchBox>::default())));
        });
    }
    {
        let a = app.clone();
        let w = ui.as_weak();
        ui.on_editor_edited(move || {
            let Some(ui) = w.upgrade() else { return };
            // tidy_editor dispatches from inside tick()'s borrow, and that
            // dispatch re-enters here — skipping then is both safe and right
            let Ok(mut app) = a.try_borrow_mut() else { return };
            app.note_edit(&ui);
        });
    }
    {
        let a = app.clone();
        // synchronous on purpose — Control must still be held (see redo)
        ui.on_redo_requested(move || {
            let ui = a.borrow().ui.upgrade().unwrap();
            redo(&ui);
        });
    }
    {
        let a = app.clone();
        ui.on_save_clicked(move || {
            let ui = a.borrow().ui.upgrade().unwrap();
            a.borrow_mut().start_export(&ui);
        });
    }
    {
        let a = app.clone();
        ui.on_cancel_export(move || {
            let ui = a.borrow().ui.upgrade().unwrap();
            let mut app = a.borrow_mut();
            app.export = None;
            ui.set_exporting(false);
            ui.set_export_progress(0.0);
            ui.set_status("Export cancelled.".into());
            app.status_until = Some(Instant::now() + Duration::from_secs(4));
        });
    }
    let timer = slint::Timer::default();
    {
        let a = app.clone();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
            a.borrow_mut().tick();
        });
    }

    ui.invoke_focus_editor();
    ui.run()?;
    Ok(())
}
