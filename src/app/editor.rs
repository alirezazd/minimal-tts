//! Editor buffer handling: auto-tidy on paste, and Ctrl+Y redo.
//!
//! Rewriting the buffer is only safe through TextInput's own edit path; see
//! `tidy_editor`.

use super::MainWindow;
use slint::ComponentHandle;
use std::time::{Duration, Instant};

/// Replay Ctrl+Y as the Ctrl+Shift+Z that Slint recognizes as redo off Windows.
pub(crate) fn redo(ui: &MainWindow) {
    use slint::platform::{Key, WindowEvent};
    let w = ui.window();
    // Runs while Ctrl+Y is still being handled, so Control is already down —
    // only Shift is added, and it is released again. Never synthesize Control
    // itself: deferring this and pressing it after the user let go left the
    // modifier stuck on, turning every later keystroke into a shortcut.
    w.dispatch_event(WindowEvent::KeyPressed { text: Key::Shift.into() });
    w.dispatch_event(WindowEvent::KeyPressed { text: "z".into() });
    w.dispatch_event(WindowEvent::KeyReleased { text: Key::Shift.into() });
}

/// Replace the whole buffer through TextInput's own edit path, so the undo
/// stack stays consistent — writing editor-text directly leaves stale offsets
/// and panics on Ctrl+Z.
pub(crate) fn replace_all(ui: &MainWindow, text: &str) {
    use slint::platform::{Key, WindowEvent};
    let w = ui.window();
    // This runs a tick after the paste that triggered it, so the Control from
    // Ctrl+V is still held — and TextInput ignores *every* text input while
    // Control is down (i-slint-core items/text.rs:1057), which made the whole
    // replacement a silent no-op and auto-tidy look dead. Release it first.
    // A stray release cannot strand a modifier the way a stray press can, and
    // the backend resyncs the real state on the next key event.
    w.dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
    ui.invoke_focus_editor();
    ui.invoke_select_all_editor();
    w.dispatch_event(WindowEvent::KeyPressed { text: text.into() });
}

/// A paste this much larger than the previous buffer is what arms auto-tidy;
/// ordinary typing never trips it.
pub(crate) const PASTE_JUMP: usize = 120;

/// Should this edit arm auto-tidy? A big jump in length means a paste — except
/// when the text is exactly what a tidy replaced, which is the user pressing
/// Ctrl+Z. Re-tidying that would make the undo impossible.
///
/// The jump is measured in either direction. Only counting growth missed the
/// most ordinary case there is: select-all and paste a document shorter than
/// the one already loaded, which shrinks the buffer and so never tidied.
pub(crate) fn arms_tidy(prev_len: usize, text: &str, tidy_source: Option<&str>) -> bool {
    text.len().abs_diff(prev_len) > PASTE_JUMP && tidy_source != Some(text)
}

/// TextInput drops any insert holding a control char other than '\n'
/// (i-slint-core items/text.rs), so a stray tab would make the whole
/// replacement a silent no-op.
pub(crate) fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .filter(|&c| c == '\n' || !c.is_control())
        .collect()
}

/// What gets read and exported: exactly what the editor shows. Tidying happens
/// when text arrives (paste, session restore), never here — the reader showing
/// something the editor doesn't is the bug this avoids.
pub(crate) fn prepared_text(ui: &MainWindow) -> String {
    ui.get_editor_text().to_string()
}

impl super::App {
    /// Every editor edit. A big jump in length is a paste; arm the tidy for the
    /// next tick rather than rewriting the buffer from inside the edit callback.
    pub(crate) fn note_edit(&mut self, ui: &MainWindow) {
        let text = ui.get_editor_text();
        if arms_tidy(self.editor_len, &text, self.tidy_source.as_deref()) {
            self.pending_tidy = true;
        } else if self.tidy_source.as_deref() == Some(text.as_str()) {
            self.tidy_source = None; // they undid it; let a later paste re-arm
        }
        self.editor_len = text.len();
    }

    /// Clean a messy paste in place, via TextInput's own edit path (select-all
    /// then a dispatched text event) so the undo stack stays consistent —
    /// writing editor-text directly leaves stale offsets and panics on Ctrl+Z.
    pub(crate) fn tidy_editor(&mut self, ui: &MainWindow) {
        if self.reading {
            return; // the editor is locked while reading
        }
        let raw = ui.get_editor_text().to_string();
        let Some(cleaned) = crate::tidy::tidy_if_messy(&raw).map(|c| sanitize(&c)) else {
            return;
        };
        self.tidy_source = Some(raw);
        replace_all(ui, &cleaned);
        self.editor_len = ui.get_editor_text().len();
        ui.set_tidy_banner(true);
        self.banner_until = Some(Instant::now() + Duration::from_secs(3));
        if ui.get_search_open() {
            self.run_search(ui, false); // offsets moved under the find bar
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MainWindow;

        #[test]
    fn tidy_arms_on_paste_but_not_on_undo() {
        let pasted = "x".repeat(500);
        assert!(arms_tidy(0, &pasted, None), "a big paste arms it");
        assert!(!arms_tidy(0, "typed a bit", None), "typing never does");
        // Ctrl+Z on a tidy restores the longer original — that must not re-arm,
        // or the undo is immediately reversed
        assert!(!arms_tidy(0, &pasted, Some(&pasted)), "undoing a tidy is not a paste");
        // ...but pasting it again afterwards should still work
        assert!(arms_tidy(0, &pasted, Some("something else")));
        // replacing a loaded document with a shorter one is still a paste:
        // hard-wrapped text pasted over a longer buffer used to keep every
        // line break, and the reader paused at each one
        assert!(arms_tidy(5000, &pasted, None), "a shorter paste arms it too");
        assert!(!arms_tidy(500, &"x".repeat(450), None), "small trims do not");
    }

        #[test]
    fn sanitize_keeps_newlines_drops_control() {
        // a surviving tab makes TextInput reject the whole insert, which would
        // leave auto-tidy silently doing nothing with the buffer fully selected
        assert_eq!(sanitize("a\tb"), "a b");
        assert_eq!(sanitize("a\r\nb"), "a\nb");
        assert_eq!(sanitize("a\u{0}\u{7}b"), "ab");
        assert_eq!(sanitize("héllo — ünïcode\n2nd line"), "héllo — ünïcode\n2nd line");
    }

    /// The real paste is Ctrl+V, and the tidy lands a tick later with Control
    /// still held. TextInput drops every text input while Control is down, so
    /// the replacement silently did nothing — auto-tidy looked completely dead
    /// even though every other test passed, because they all dispatched with no
    /// modifier held.

        #[test]
    fn tidy_applies_while_ctrl_is_still_held() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().unwrap();
        let messy = "the rows are individual cycles and adjacent cycles are almost\n\
                     identical, the ADC signal is smooth, and multi-cycle\n\
                     instructions and stalls make the same feature row repeat\n\
                     across consecutive cycles, so pooling them and splitting\n\
                     at random would leak neighbours into the training set.";
        ui.set_editor_text(messy.into());
        let cleaned = sanitize(&crate::tidy::tidy(messy));
        assert!(!cleaned.contains('\n'), "the wrapped lines join into one");

        // exactly what the app sees mid-paste: Control down, never released
        use slint::platform::{Key, WindowEvent};
        ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });

        replace_all(&ui, &cleaned);
        assert_eq!(
            ui.get_editor_text().as_str(),
            cleaned,
            "auto-tidy must apply even with Ctrl still down from Ctrl+V"
        );

        // and the buffer must still be editable afterwards, not left in a
        // state where the released modifier broke ordinary typing
        ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
        ui.window().dispatch_event(WindowEvent::KeyPressed { text: "!".into() });
        assert!(ui.get_editor_text().contains('!'), "typing still works");
    }

    /// Auto-tidy rewrites the live buffer, which is only safe through
    /// TextInput's own edit path. Covers the replacement, Ctrl+Z back to the
    /// messy original, and Ctrl+Y forward again.

        #[test]
    fn tidy_replacement_undoes_and_redoes() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().unwrap();
        let messy = "Model IR channels and proc state as first-class graph\n\
                     nodes connected by synthetic binding edges, so channel\n\
                     and state edits flow through the existing node and edge\n\
                     differencing and patching machinery [12] and are enacted\n\
                     through native channel and state APIs, removing\n\
                     the manual fixup step.";
        ui.set_editor_text(messy.into());
        assert!(crate::tidy::looks_messy(messy));
        let cleaned = sanitize(&crate::tidy::tidy(messy));

        ui.invoke_focus_editor();
        ui.invoke_select_all_editor();
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: cleaned.clone().into() });
        assert_eq!(ui.get_editor_text().as_str(), cleaned, "buffer holds the tidied text");
        assert!(!ui.get_editor_text().contains("graph\nnodes"), "line breaks joined");

        // insert() deletes the selection first, so the rewrite is two undo steps
        let key = |t: slint::SharedString| {
            ui.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: t });
        };
        let release = |t: slint::SharedString| {
            ui.window().dispatch_event(slint::platform::WindowEvent::KeyReleased { text: t });
        };
        use slint::platform::Key;
        for _ in 0..2 {
            key(Key::Control.into());
            key("z".into());
            release(Key::Control.into());
        }
        assert_eq!(ui.get_editor_text().as_str(), messy, "Ctrl+Z restores the paste");

        // Ctrl+Y is Windows-only in Slint; the app replays it as Ctrl+Shift+Z.
        // Drive it through the real key path — redo() relies on Control being
        // held by the keystroke that triggered it.
        let w = ui.as_weak();
        ui.on_redo_requested(move || redo(&w.upgrade().unwrap()));
        for _ in 0..2 {
            key(Key::Control.into());
            key("y".into());
            release(Key::Control.into());
        }
        assert_eq!(ui.get_editor_text().as_str(), cleaned, "Ctrl+Y redoes the tidy");
    }

    /// Ctrl+Y must redo *and* leave the modifier state clean. Synthesizing
    /// Control here once left it stuck down, so every following keystroke
    /// became a shortcut and the window closed.

        #[test]
    fn ctrl_y_redoes_without_stranding_modifiers() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().unwrap();
        // wire it exactly as run() does
        let w = ui.as_weak();
        ui.on_redo_requested(move || redo(&w.upgrade().unwrap()));
        ui.invoke_focus_editor();

        use slint::platform::{Key, WindowEvent};
        let press = |t: slint::SharedString| {
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: t })
        };
        let release = |t: slint::SharedString| {
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: t })
        };

        press("h".into());
        press("i".into());
        assert_eq!(ui.get_editor_text().as_str(), "hi");
        press(Key::Control.into());
        press("z".into());
        release(Key::Control.into());
        let after_undo = ui.get_editor_text().to_string();
        assert_ne!(after_undo, "hi", "Ctrl+Z undid something");

        // Ctrl+Y, with the real key sequence including the release
        press(Key::Control.into());
        press("y".into());
        release(Key::Control.into());
        assert_eq!(ui.get_editor_text().as_str(), "hi", "Ctrl+Y redid the edit");

        // the regression: a plain keystroke must now type a character rather
        // than act as a shortcut (a latched Control made "!" a no-op here)
        press("!".into());
        assert!(
            ui.get_editor_text().contains('!'),
            "Control must not stay latched after Ctrl+Y — got {:?}",
            ui.get_editor_text()
        );
    }
}
