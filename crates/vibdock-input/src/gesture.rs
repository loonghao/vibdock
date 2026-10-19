//! Physical gesture classification for one control.
//!
//! The platform adapters only report press and release edges. This module turns
//! those edges into at most one gesture per press sequence. It is deliberately
//! platform independent and uses caller-supplied monotonic milliseconds, so the
//! Windows hook and the HID gamepad monitor can share it and the timing rules
//! can be tested without devices.
//!
//! The host wires this in with the gesture layer; until then the module is kept
//! compiled and tested but not constructed by the service.

/// One logical action selected by a press sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    Single,
    Double,
    Long,
}

/// Timing bounds chosen to match the workbench's user-facing sliders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timings {
    pub double_window_ms: u64,
    pub long_press_ms: u64,
}

impl Timings {
    pub const fn new(double_window_ms: u64, long_press_ms: u64) -> Option<Self> {
        // A double-click window shorter than 50 ms is not reliably producible by
        // a human; a long press below 150 ms would collide with ordinary taps.
        if double_window_ms < 50
            || double_window_ms > 2_000
            || long_press_ms < 150
            || long_press_ms > 5_000
        {
            return None;
        }
        Some(Self {
            double_window_ms,
            long_press_ms,
        })
    }
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            double_window_ms: 300,
            long_press_ms: 550,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Held { since_ms: u64, long_fired: bool },
    AwaitingDouble { deadline_ms: u64 },
    Consumed,
}

pub struct Classifier {
    timings: Timings,
    state: State,
}

impl Classifier {
    pub fn new(timings: Timings) -> Self {
        Self {
            timings,
            state: State::Idle,
        }
    }

    pub fn timings(&self) -> Timings {
        self.timings
    }

    pub fn is_idle(&self) -> bool {
        matches!(self.state, State::Idle)
    }

    pub fn reset(&mut self) {
        self.state = State::Idle;
    }

    /// Records a key-down edge. Returns `Double` on the second press of a pair.
    pub fn press(&mut self, now_ms: u64) -> Option<Gesture> {
        match self.state {
            State::Idle => {
                self.state = State::Held {
                    since_ms: now_ms,
                    long_fired: false,
                };
                None
            }
            // The driver may repeat a down report while the key is held.
            State::Held { .. } | State::Consumed => None,
            State::AwaitingDouble { deadline_ms } => {
                if now_ms <= deadline_ms {
                    self.state = State::Consumed;
                    Some(Gesture::Double)
                } else {
                    // The caller did not tick at the deadline. Report the
                    // expired single press and treat this edge as a new press.
                    self.state = State::Held {
                        since_ms: now_ms,
                        long_fired: false,
                    };
                    Some(Gesture::Single)
                }
            }
        }
    }

    /// Records a key-up edge. A long press that already fired is not repeated.
    pub fn release(&mut self, now_ms: u64) -> Option<Gesture> {
        match self.state {
            State::Idle | State::Consumed => {
                self.state = State::Idle;
                None
            }
            State::Held {
                since_ms,
                long_fired,
            } => {
                if long_fired {
                    self.state = State::Idle;
                    return None;
                }
                // A caller that never ticked while held still gets the long
                // gesture, just late. The host normally ticks promptly.
                if now_ms.saturating_sub(since_ms) >= self.timings.long_press_ms {
                    self.state = State::Idle;
                    return Some(Gesture::Long);
                }
                if self.timings.double_window_ms == 0 {
                    self.state = State::Idle;
                    return Some(Gesture::Single);
                }
                self.state = State::AwaitingDouble {
                    deadline_ms: now_ms.saturating_add(self.timings.double_window_ms),
                };
                None
            }
            State::AwaitingDouble { .. } => None,
        }
    }

    /// Advances timers. Call this often enough to honour the long threshold and
    /// to resolve a single click after the double window.
    pub fn tick(&mut self, now_ms: u64) -> Option<Gesture> {
        match self.state {
            State::Held {
                since_ms,
                long_fired: false,
            } if now_ms.saturating_sub(since_ms) >= self.timings.long_press_ms => {
                self.state = State::Held {
                    since_ms,
                    long_fired: true,
                };
                Some(Gesture::Long)
            }
            State::AwaitingDouble { deadline_ms } if now_ms >= deadline_ms => {
                self.state = State::Idle;
                Some(Gesture::Single)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier() -> Classifier {
        Classifier::new(Timings::default())
    }

    #[test]
    fn a_short_press_resolves_to_single_after_the_double_window() {
        let mut classifier = classifier();
        assert_eq!(classifier.press(0), None);
        assert_eq!(classifier.release(100), None);
        assert_eq!(classifier.tick(399), None);
        assert_eq!(classifier.tick(400), Some(Gesture::Single));
        assert!(classifier.is_idle());
        // A resolved single never repeats on later ticks.
        assert_eq!(classifier.tick(900), None);
    }

    #[test]
    fn a_second_press_inside_the_window_is_double_and_release_is_silent() {
        let mut classifier = classifier();
        classifier.press(0);
        classifier.release(100);
        assert_eq!(classifier.press(300), Some(Gesture::Double));
        assert_eq!(classifier.release(380), None);
        assert_eq!(classifier.tick(1_000), None);
        assert!(classifier.is_idle());
    }

    #[test]
    fn holding_past_the_threshold_fires_long_once_and_release_does_not_fire_single() {
        let mut classifier = classifier();
        classifier.press(0);
        assert_eq!(classifier.tick(549), None);
        assert_eq!(classifier.tick(550), Some(Gesture::Long));
        assert_eq!(classifier.tick(900), None);
        assert_eq!(classifier.release(1_000), None);
        assert!(classifier.is_idle());
    }

    #[test]
    fn a_late_release_still_reports_long_if_tick_was_missed() {
        let mut classifier = classifier();
        classifier.press(0);
        assert_eq!(classifier.release(700), Some(Gesture::Long));
        assert!(classifier.is_idle());
    }

    #[test]
    fn repeated_down_reports_do_not_create_an_extra_gesture() {
        let mut classifier = classifier();
        classifier.press(0);
        classifier.press(10);
        classifier.press(20);
        assert_eq!(classifier.release(100), None);
        assert_eq!(classifier.tick(400), Some(Gesture::Single));
    }

    #[test]
    fn two_presses_without_a_release_are_still_ignored() {
        let mut classifier = classifier();
        classifier.press(0);
        assert_eq!(classifier.press(100), None);
        assert_eq!(classifier.release(200), None);
        assert_eq!(classifier.tick(500), Some(Gesture::Single));
    }

    #[test]
    fn a_second_press_after_the_window_starts_a_new_sequence() {
        let mut classifier = classifier();
        classifier.press(0);
        classifier.release(100);
        // The deadline was 400 ms; this press is too late.
        assert_eq!(classifier.press(500), Some(Gesture::Single));
        assert_eq!(classifier.release(560), None);
        assert_eq!(classifier.tick(860), Some(Gesture::Single));
    }

    #[test]
    fn a_double_does_not_also_fire_long_while_the_second_press_is_held() {
        let mut classifier = classifier();
        classifier.press(0);
        classifier.release(80);
        assert_eq!(classifier.press(200), Some(Gesture::Double));
        assert_eq!(classifier.tick(900), None);
        assert_eq!(classifier.release(950), None);
        assert!(classifier.is_idle());
    }

    #[test]
    fn reset_abandons_any_pending_single() {
        let mut classifier = classifier();
        classifier.press(0);
        classifier.release(50);
        classifier.reset();
        assert!(classifier.is_idle());
        assert_eq!(classifier.tick(500), None);
    }

    #[test]
    fn timings_reject_unusable_values() {
        assert!(Timings::new(49, 500).is_none());
        assert!(Timings::new(300, 149).is_none());
        assert!(Timings::new(2_001, 500).is_none());
        assert!(Timings::new(300, 5_001).is_none());
        assert_eq!(
            Timings::new(300, 550),
            Some(Timings {
                double_window_ms: 300,
                long_press_ms: 550
            })
        );
    }

    #[test]
    fn a_zero_window_presses_are_not_representable_but_release_stays_single() {
        // Timings::new rejects zero, but a deserialised or future value of zero
        // must still not deadlock a release: it resolves immediately.
        let mut classifier = Classifier::new(Timings {
            double_window_ms: 0,
            long_press_ms: 550,
        });
        classifier.press(0);
        assert_eq!(classifier.release(100), Some(Gesture::Single));
        assert!(classifier.is_idle());
    }
}
