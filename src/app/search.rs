//! Ctrl+F: find in the editor buffer and in the read-along view.

use super::{PAD_Y, SearchBox};
use crate::MainWindow;
use slint::{ModelRc, VecModel};
use std::rc::Rc;
use std::time::{Duration, Instant};

// Ctrl+F: case-insensitive (ASCII) byte-range matches, capped so a one-letter
// query over 25k chars can't flood the highlight model.
pub(crate) const MAX_SEARCH_HITS: usize = 400;

pub(crate) fn find_hits(hay: &str, needle: &str) -> Vec<(usize, usize)> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    let mut out = Vec::new();
    if n.is_empty() {
        return out;
    }
    let mut i = 0;
    while i + n.len() <= h.len() && out.len() < MAX_SEARCH_HITS {
        if h[i..i + n.len()].eq_ignore_ascii_case(n) {
            out.push((i, i + n.len()));
            i += n.len();
        } else {
            i += 1;
        }
    }
    out
}

impl super::App {
    /// Recompute Ctrl+F hits against whatever text is active (reader doc or
    /// editor buffer). `jump` scrolls/selects the first hit.
    pub(crate) fn run_search(&mut self, ui: &MainWindow, jump: bool) {
        let hay = if self.reading {
            self.doc.as_ref().map(|d| d.raw.clone()).unwrap_or_default()
        } else {
            ui.get_editor_text().to_string()
        };
        self.search_hits = find_hits(&hay, &self.search_query);
        self.search_idx = 0;
        self.apply_search(ui, jump);
    }

    pub(crate) fn search_nav(&mut self, ui: &MainWindow, dir: i32) {
        let n = self.search_hits.len();
        if n == 0 {
            return;
        }
        self.search_idx = (self.search_idx as i64 + dir as i64).rem_euclid(n as i64) as usize;
        self.apply_search(ui, true);
    }

    /// Push count + highlights to the UI; optionally bring the current hit into view.
    pub(crate) fn apply_search(&mut self, ui: &MainWindow, jump: bool) {
        ui.set_search_count(if self.search_query.is_empty() {
            "".into()
        } else if self.search_hits.is_empty() {
            "0".into()
        } else {
            let cap = if self.search_hits.len() >= MAX_SEARCH_HITS { "+" } else { "" };
            format!("{}/{}{cap}", self.search_idx + 1, self.search_hits.len()).into()
        });
        if !self.reading {
            ui.set_search_boxes(ModelRc::from(Rc::new(VecModel::<SearchBox>::default())));
            if jump {
                if let Some(&(a, b)) = self.search_hits.get(self.search_idx) {
                    // selection doubles as the highlight; its cursor-position-changed
                    // handler scrolls the match into view
                    ui.invoke_select_editor_range(a as i32, b as i32);
                }
            }
            return;
        }
        // reader: a wash over every word a hit touches (hits and word ranges are
        // both sorted, so a single forward sweep maps them)
        let mut boxes = Vec::new();
        let mut cur_y: Option<f32> = None;
        if let Some(doc) = self.doc.as_ref() {
            let mut wi = 0usize;
            for (hi, &(a, b)) in self.search_hits.iter().enumerate() {
                while wi < doc.word_range.len() && doc.word_range[wi].1 <= a {
                    wi += 1;
                }
                let mut wj = wi;
                while wj < doc.word_range.len() && doc.word_range[wj].0 < b {
                    let w = &doc.ui_words[wj];
                    boxes.push(SearchBox {
                        x: w.x,
                        y: w.y,
                        w: w.w,
                        h: w.h,
                        current: hi == self.search_idx,
                    });
                    if hi == self.search_idx && cur_y.is_none() {
                        cur_y = Some(w.y);
                    }
                    wj += 1;
                }
            }
        }
        ui.set_search_boxes(ModelRc::from(Rc::new(VecModel::from(boxes))));
        if jump {
            if let Some(y) = cur_y {
                // jump straight there; hold auto-follow off briefly so playback
                // doesn't immediately yank the view back to the spoken word
                let view_h = ui.get_reader_height();
                let doc_h = ui.get_doc_height();
                let want = -((y + PAD_Y - view_h * 0.45).clamp(0.0, (doc_h - view_h).max(0.0)));
                ui.set_reader_scroll(want);
                self.last_scroll_set = want;
                self.scroll_target = None;
                self.user_scroll_until = Instant::now() + Duration::from_secs(4);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

        #[test]
    fn find_hits_case_insensitive_and_bounded() {
        assert_eq!(find_hits("The theory of Theft", "the"), vec![(0, 3), (4, 7), (14, 17)]);
        assert_eq!(find_hits("aaaa", "aa"), vec![(0, 2), (2, 4)], "non-overlapping");
        assert!(find_hits("anything", "").is_empty());
        let flood = "e ".repeat(2000);
        assert_eq!(find_hits(&flood, "e").len(), MAX_SEARCH_HITS);
        // matches land on char boundaries even with multibyte neighbours
        let s = "café CAFÉ cafe";
        for (a, b) in find_hits(s, "café") {
            assert!(s.is_char_boundary(a) && s.is_char_boundary(b));
        }
    }

    // The fix that mattered most: sentence_ranges must never slice mid-UTF-8.
}
