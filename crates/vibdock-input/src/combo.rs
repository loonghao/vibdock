//! Multi-button combo recognition.
//!
//! A combo is a configured set of controls that is either tapped in quick
//! succession (`Sequence`) or held together past a threshold (`Hold`). The
//! matcher consumes the same press/release edges as [`crate::gesture`] and is
//! platform independent, so the Windows sentinel-key hook and the HID gamepad
//! monitor can feed it without duplicating timing rules.
//!
//! Ambiguity is deliberate: if both `1+2` and `1+2+3` are configured, a quick
//! `1,2` waits until the chord window after the last press before resolving to
//! `1+2`, so the user can still add `3`. A longer combo always wins.
//!
//! The module is compiled and tested while the profile integration is pending;
//! the service does not construct it yet.

use std::collections::BTreeMap;

pub const MAX_CONTROLS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComboKind {
    /// Distinct controls tapped within the chord window.
    Sequence,
    /// Distinct controls held together past the hold threshold.
    Hold,
}

/// A configured combo. Controls are sorted and de-duplicated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    controls: Vec<u8>,
    kind: ComboKind,
}

impl Pattern {
    pub fn new(kind: ComboKind, mut controls: Vec<u8>) -> Option<Self> {
        controls.sort_unstable();
        controls.dedup();
        if controls.len() < 2 || controls.len() > MAX_CONTROLS {
            return None;
        }
        Some(Self { controls, kind })
    }

    pub fn controls(&self) -> &[u8] {
        &self.controls
    }

    pub fn kind(&self) -> ComboKind {
        self.kind
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub chord_window_ms: u64,
    pub hold_ms: u64,
}

impl Config {
    pub const fn new(chord_window_ms: u64, hold_ms: u64) -> Option<Self> {
        if chord_window_ms < 80 || chord_window_ms > 1_000 || hold_ms < 200 || hold_ms > 3_000 {
            return None;
        }
        Some(Self {
            chord_window_ms,
            hold_ms,
        })
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            chord_window_ms: 250,
            hold_ms: 550,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub controls: Vec<u8>,
    pub kind: ComboKind,
}

#[derive(Clone, Copy, Debug)]
struct Held {
    since_ms: u64,
}

#[derive(Clone, Debug)]
struct Pending {
    hit: Hit,
    deadline_ms: u64,
}

pub struct Matcher {
    config: Config,
    patterns: Vec<Pattern>,
    held: BTreeMap<u8, Held>,
    /// Distinct controls pressed since the current sequence started.
    order: Vec<u8>,
    first_press_ms: Option<u64>,
    last_press_ms: Option<u64>,
    pending: Option<Pending>,
    /// A hold fired; its release must not be reinterpreted as a tap.
    consumed: bool,
}

impl Matcher {
    pub fn new(config: Config, patterns: Vec<Pattern>) -> Self {
        Self {
            config,
            patterns,
            held: BTreeMap::new(),
            order: Vec::new(),
            first_press_ms: None,
            last_press_ms: None,
            pending: None,
            consumed: false,
        }
    }

    pub fn config(&self) -> Config {
        self.config
    }

    pub fn is_idle(&self) -> bool {
        self.held.is_empty() && self.order.is_empty() && self.pending.is_none()
    }

    pub fn reset(&mut self) {
        self.held.clear();
        self.order.clear();
        self.first_press_ms = None;
        self.last_press_ms = None;
        self.pending = None;
        self.consumed = false;
    }

    /// Records a control press. A hold is emitted by [`Self::tick`] once the
    /// threshold is crossed; a sequence is resolved on the final release.
    pub fn press(&mut self, control: u8, now_ms: u64) -> Option<Hit> {
        if self.consumed || self.held.contains_key(&control) {
            return None;
        }
        if let Some(first) = self.first_press_ms {
            if now_ms.saturating_sub(first) > self.config.chord_window_ms {
                self.order.clear();
                self.pending = None;
                self.first_press_ms = Some(now_ms);
            }
        } else {
            self.first_press_ms = Some(now_ms);
        }
        if !self.order.contains(&control) {
            self.order.push(control);
        }
        self.last_press_ms = Some(now_ms);
        self.pending = None;
        self.held.insert(control, Held { since_ms: now_ms });
        None
    }

    /// Records a control release. The hit is returned when the last control of
    /// a tapped combo is released and no longer combo could still extend it.
    pub fn release(&mut self, control: u8, now_ms: u64) -> Option<Hit> {
        self.held.remove(&control)?;
        if !self.held.is_empty() {
            return None;
        }
        if self.consumed {
            self.order.clear();
            self.first_press_ms = None;
            self.last_press_ms = None;
            self.consumed = false;
            return None;
        }
        let candidate = self.sequence_candidate().cloned();
        match candidate {
            Some(pattern) if self.has_longer_sequence(&pattern) => {
                // Wait for the user to add the final control. If the window has
                // already elapsed, resolve immediately.
                let hit = hit_for(&pattern);
                let deadline = self
                    .last_press_ms
                    .unwrap_or(now_ms)
                    .saturating_add(self.config.chord_window_ms);
                if now_ms >= deadline {
                    self.finish_sequence();
                    Some(hit)
                } else {
                    self.pending = Some(Pending {
                        hit,
                        deadline_ms: deadline,
                    });
                    None
                }
            }
            Some(pattern) => {
                let hit = hit_for(&pattern);
                self.finish_sequence();
                Some(hit)
            }
            None => {
                // A first tapped control may still be extended by another
                // control inside the chord window. Keep it only if some
                // configured sequence could still use it.
                if !self.could_extend_sequence() {
                    self.finish_sequence();
                }
                None
            }
        }
    }

    /// Fires a hold as soon as all of its controls have been held past the
    /// threshold, or resolves a pending shorter sequence at its deadline.
    pub fn tick(&mut self, now_ms: u64) -> Option<Hit> {
        if !self.consumed {
            if let Some(pattern) = self.hold_candidate(now_ms).cloned() {
                self.consumed = true;
                self.pending = None;
                self.order.clear();
                return Some(hit_for(&pattern));
            }
        }
        if let Some(pending) = self.pending.clone() {
            if now_ms >= pending.deadline_ms {
                self.pending = None;
                self.finish_sequence();
                return Some(pending.hit);
            }
        }
        if self.held.is_empty()
            && !self.order.is_empty()
            && self.pending.is_none()
            && self
                .last_press_ms
                .is_some_and(|last| now_ms.saturating_sub(last) >= self.config.chord_window_ms)
        {
            self.finish_sequence();
        }
        None
    }

    fn could_extend_sequence(&self) -> bool {
        self.patterns.iter().any(|pattern| {
            pattern.kind == ComboKind::Sequence
                && pattern.controls.len() > self.order.len()
                && self
                    .order
                    .iter()
                    .all(|control| pattern.controls.contains(control))
        })
    }

    fn sequence_candidate(&self) -> Option<&Pattern> {
        let mut controls = self.order.clone();
        controls.sort_unstable();
        controls.dedup();
        self.patterns
            .iter()
            .find(|pattern| pattern.kind == ComboKind::Sequence && pattern.controls == controls)
    }

    fn has_longer_sequence(&self, current: &Pattern) -> bool {
        self.patterns.iter().any(|pattern| {
            pattern.kind == ComboKind::Sequence
                && pattern.controls.len() > current.controls.len()
                && current
                    .controls
                    .iter()
                    .all(|control| pattern.controls.contains(control))
        })
    }

    fn hold_candidate(&self, now_ms: u64) -> Option<&Pattern> {
        self.patterns
            .iter()
            .filter(|pattern| pattern.kind == ComboKind::Hold)
            .filter(|pattern| {
                pattern.controls.iter().all(|control| {
                    self.held.get(control).is_some_and(|held| {
                        now_ms.saturating_sub(held.since_ms) >= self.config.hold_ms
                    })
                })
            })
            // Longest matching hold wins if two patterns overlap.
            .max_by_key(|pattern| pattern.controls.len())
    }

    fn finish_sequence(&mut self) {
        self.order.clear();
        self.first_press_ms = None;
        self.last_press_ms = None;
        self.pending = None;
        self.consumed = false;
    }
}

fn hit_for(pattern: &Pattern) -> Hit {
    Hit {
        controls: pattern.controls.clone(),
        kind: pattern.kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sequence(controls: &[u8]) -> Pattern {
        Pattern::new(ComboKind::Sequence, controls.to_vec()).unwrap()
    }

    fn hold(controls: &[u8]) -> Pattern {
        Pattern::new(ComboKind::Hold, controls.to_vec()).unwrap()
    }

    fn matcher(patterns: Vec<Pattern>) -> Matcher {
        Matcher::new(Config::default(), patterns)
    }

    #[test]
    fn one_two_tapped_is_a_sequence() {
        let mut matcher = matcher(vec![sequence(&[1, 2])]);
        assert_eq!(matcher.press(1, 0), None);
        assert_eq!(matcher.release(1, 40), None);
        assert_eq!(matcher.press(2, 80), None);
        assert_eq!(
            matcher.release(2, 120),
            Some(Hit {
                controls: vec![1, 2],
                kind: ComboKind::Sequence
            })
        );
        assert!(matcher.is_idle());
    }

    #[test]
    fn two_three_tapped_is_order_independent() {
        let mut matcher = matcher(vec![sequence(&[2, 3])]);
        matcher.press(3, 0);
        matcher.release(3, 30);
        matcher.press(2, 70);
        assert_eq!(
            matcher.release(2, 110),
            Some(Hit {
                controls: vec![2, 3],
                kind: ComboKind::Sequence
            })
        );
    }

    #[test]
    fn one_two_three_tapped_matches_the_longer_sequence() {
        let mut matcher = matcher(vec![sequence(&[1, 2, 3])]);
        matcher.press(1, 0);
        matcher.release(1, 30);
        matcher.press(2, 60);
        matcher.release(2, 90);
        matcher.press(3, 120);
        assert_eq!(
            matcher.release(3, 150),
            Some(Hit {
                controls: vec![1, 2, 3],
                kind: ComboKind::Sequence
            })
        );
    }

    #[test]
    fn a_shorter_sequence_waits_for_a_possible_longer_one() {
        let mut matcher = matcher(vec![sequence(&[1, 2]), sequence(&[1, 2, 3])]);
        matcher.press(1, 0);
        matcher.release(1, 30);
        matcher.press(2, 60);
        // Both released; 1+2 is a candidate but 1+2+3 is still possible.
        assert_eq!(matcher.release(2, 90), None);
        assert_eq!(matcher.tick(300), None);
        assert_eq!(
            matcher.tick(310),
            Some(Hit {
                controls: vec![1, 2],
                kind: ComboKind::Sequence
            })
        );
    }

    #[test]
    fn adding_the_third_control_wins_over_the_shorter_sequence() {
        let mut matcher = matcher(vec![sequence(&[1, 2]), sequence(&[1, 2, 3])]);
        matcher.press(1, 0);
        matcher.release(1, 30);
        matcher.press(2, 60);
        assert_eq!(matcher.release(2, 90), None);
        matcher.press(3, 120);
        assert_eq!(
            matcher.release(3, 150),
            Some(Hit {
                controls: vec![1, 2, 3],
                kind: ComboKind::Sequence
            })
        );
    }

    #[test]
    fn holding_one_and_two_fires_the_hold_and_not_a_sequence() {
        let mut matcher = matcher(vec![hold(&[1, 2])]);
        matcher.press(1, 0);
        matcher.press(2, 80);
        assert_eq!(matcher.tick(500), None);
        assert_eq!(
            matcher.tick(630),
            Some(Hit {
                controls: vec![1, 2],
                kind: ComboKind::Hold
            })
        );
        // The release that follows must not turn the hold into a tap.
        assert_eq!(matcher.release(1, 700), None);
        assert_eq!(matcher.release(2, 720), None);
        assert_eq!(matcher.tick(900), None);
        assert!(matcher.is_idle());
    }

    #[test]
    fn a_quick_release_before_the_hold_threshold_stays_a_sequence() {
        let mut matcher = matcher(vec![hold(&[1, 2]), sequence(&[1, 2])]);
        matcher.press(1, 0);
        matcher.press(2, 50);
        assert_eq!(matcher.tick(300), None);
        assert_eq!(matcher.release(1, 320), None);
        assert_eq!(
            matcher.release(2, 350),
            Some(Hit {
                controls: vec![1, 2],
                kind: ComboKind::Sequence
            })
        );
    }

    #[test]
    fn a_tap_outside_the_chord_window_does_not_match() {
        let mut matcher = matcher(vec![sequence(&[1, 2])]);
        matcher.press(1, 0);
        matcher.release(1, 40);
        // The second press is outside the 250 ms window.
        matcher.press(2, 300);
        assert_eq!(matcher.release(2, 340), None);
        // 2 could still be followed by 1 inside a new window; after the window
        // expires the buffered order is discarded.
        assert_eq!(matcher.tick(600), None);
        assert!(matcher.is_idle());
    }

    #[test]
    fn repeated_presses_are_not_double_counted() {
        let mut matcher = matcher(vec![sequence(&[1, 2])]);
        matcher.press(1, 0);
        matcher.press(1, 10);
        matcher.release(1, 40);
        matcher.press(2, 80);
        assert_eq!(
            matcher.release(2, 110),
            Some(Hit {
                controls: vec![1, 2],
                kind: ComboKind::Sequence
            })
        );
    }

    #[test]
    fn invalid_patterns_are_rejected() {
        assert!(Pattern::new(ComboKind::Sequence, vec![1]).is_none());
        assert!(Pattern::new(ComboKind::Sequence, vec![1, 1]).is_none());
        assert!(Pattern::new(ComboKind::Sequence, vec![1, 2, 3, 4]).is_none());
        assert!(Pattern::new(ComboKind::Hold, vec![2, 2]).is_none());
        assert!(Config::new(79, 550).is_none());
        assert!(Config::new(250, 199).is_none());
        assert!(Config::new(250, 550).is_some());
    }
}
