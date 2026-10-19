//! Windows sentinel-key monitor for AU05 single/double/long gestures.
//!
//! Gesture mode writes `Ctrl+Alt+Shift+F10`/`F11`/`F12` into the three AU05
//! buttons. This monitor polls their global key state, feeds press/release edges
//! into the platform-independent classifier, and returns matched gestures. The
//! current implementation does not suppress the chord, so a future low-level
//! hook can replace the poll without changing the classifier or action
//! execution. AU05 cannot represent F13-F15, hence the modified F10-F12 chord.

#![cfg(windows)]

use crate::actions::gestures::GestureKind;
use crate::gesture::{Classifier, Gesture, Timings};
use std::time::Instant;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

pub(crate) struct Monitor {
    start: Instant,
    buttons: Vec<u8>,
    last: [bool; 3],
    classifiers: [Classifier; 3],
}

impl Monitor {
    pub(crate) fn new() -> Self {
        Self {
            start: Instant::now(),
            buttons: Vec::new(),
            last: [false; 3],
            classifiers: [
                Classifier::new(Timings::default()),
                Classifier::new(Timings::default()),
                Classifier::new(Timings::default()),
            ],
        }
    }

    /// Update the active sentinel buttons. Resetting the classifiers when the
    /// set changes avoids carrying a half-finished gesture across an edit.
    pub(crate) fn set_buttons(&mut self, buttons: &[u8]) {
        let mut next: Vec<u8> = buttons.iter().copied().filter(|b| *b <= 2).collect();
        next.sort_unstable();
        next.dedup();
        if next != self.buttons {
            self.buttons = next;
            for classifier in &mut self.classifiers {
                classifier.reset();
            }
            self.last = [false; 3];
        }
    }

    /// Poll the sentinel key states and return completed gestures.
    pub(crate) fn poll(&mut self) -> Vec<(u8, GestureKind)> {
        let now = self.start.elapsed().as_millis() as u64;
        let mut hits = Vec::new();
        for &button in &self.buttons {
            let virtual_key = 0x79 + i32::from(button); // VK_F10..VK_F12
                                                        // SAFETY: GetAsyncKeyState only reads the process-wide key state.
            let function_down = unsafe { GetAsyncKeyState(virtual_key) < 0 };
            let modifiers_down = unsafe {
                GetAsyncKeyState(0x11) < 0
                    && GetAsyncKeyState(0x12) < 0
                    && GetAsyncKeyState(0x10) < 0
            };
            let down = function_down && modifiers_down;
            let classifier = &mut self.classifiers[usize::from(button)];
            if down && !self.last[usize::from(button)] {
                if let Some(gesture) = classifier.press(now) {
                    hits.push((button, kind(gesture)));
                }
            } else if !down && self.last[usize::from(button)] {
                if let Some(gesture) = classifier.release(now) {
                    hits.push((button, kind(gesture)));
                }
            }
            self.last[usize::from(button)] = down;
            if let Some(gesture) = classifier.tick(now) {
                hits.push((button, kind(gesture)));
            }
        }
        hits
    }
}

fn kind(gesture: Gesture) -> GestureKind {
    match gesture {
        Gesture::Single => GestureKind::Single,
        Gesture::Double => GestureKind::Double,
        Gesture::Long => GestureKind::Long,
    }
}
