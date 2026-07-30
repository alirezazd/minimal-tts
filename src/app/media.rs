//! MPRIS media-key integration.

use crate::MainWindow;

impl super::App {
    pub(crate) fn media_playback(&mut self, playing: bool) {
        use souvlaki::MediaPlayback;
        if let Some(m) = self.media.as_mut() {
            let _ = m.set_playback(if !self.reading {
                MediaPlayback::Stopped
            } else if playing {
                MediaPlayback::Playing { progress: None }
            } else {
                MediaPlayback::Paused { progress: None }
            });
        }
    }

    pub(crate) fn drain_media(&mut self, ui: &MainWindow) {
        use souvlaki::MediaControlEvent as E;
        let mut events = Vec::new();
        if let Some(rx) = self.media_rx.as_ref() {
            while let Ok(e) = rx.try_recv() {
                events.push(e);
            }
        }
        for e in events {
            match e {
                E::Play => {
                    if self.reading {
                        self.set_playing(ui, true);
                    } else {
                        self.enter_read(ui);
                    }
                }
                E::Pause => self.set_playing(ui, false),
                E::Toggle => self.toggle(ui),
                E::Stop => {
                    if self.reading {
                        self.enter_edit(ui);
                    }
                }
                E::Next => self.nav(ui, 1),
                E::Previous => self.nav(ui, -1),
                _ => {}
            }
        }
    }
}
