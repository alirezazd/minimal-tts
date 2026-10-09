//! OS media keys: MPRIS on Linux, System Media Transport Controls on Windows.

use crate::MainWindow;
use std::ffi::c_void;

/// The native window SMTC attaches to. Only Windows needs one, and it exists
/// only once the window has been realized — hence the lazy attach.
#[cfg(windows)]
fn native_window(ui: &MainWindow) -> Option<*mut c_void> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::ComponentHandle;
    match ui.window().window_handle().window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as *mut c_void),
        _ => None,
    }
}

#[cfg(not(windows))]
fn native_window(_: &MainWindow) -> Option<*mut c_void> {
    None
}

impl super::App {
    /// Attach once, from the tick. Retried until Windows hands over its HWND;
    /// a failure (no D-Bus session, say) just leaves the media keys off.
    pub(crate) fn init_media(&mut self, ui: &MainWindow) {
        if self.media_tried {
            return;
        }
        let hwnd = native_window(ui);
        if cfg!(windows) && hwnd.is_none() {
            return;
        }
        self.media_tried = true;
        let config = souvlaki::PlatformConfig { dbus_name: "minimal_tts", display_name: "Minimal TTS", hwnd };
        let Ok(mut controls) = souvlaki::MediaControls::new(config) else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        if controls.attach(move |e| drop(tx.send(e))).is_ok() {
            self.media = Some(controls);
            self.media_rx = Some(rx);
        }
    }

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
