//! Bounded physical-key dispatch to one captured foreground process; never app-state proof.

use crate::hotkey::Chord;
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{FILETIME, HANDLE, HWND, WAIT_TIMEOUT},
    System::Threading::{
        GetProcessTimes, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE,
    },
    UI::{
        Input::KeyboardAndMouse::*,
        WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, IsWindow},
    },
};

const MAX_STEPS: usize = 32;
const MAX_DELAY_MS: u64 = 2_000;
const MAX_TOTAL_DELAY_MS: u64 = 10_000;
const MAX_RUNTIME: Duration = Duration::from_secs(15);
const RELEASE_WAIT: Duration = Duration::from_millis(750);

#[derive(Debug)]
pub(crate) struct ForegroundTarget {
    pub(crate) hwnd: isize,
    pub(crate) pid: u32,
    thread: u32,
    created: u64,
    layout: isize,
    process: OwnedHandle,
}

fn process_created(handle: HANDLE) -> Result<u64, String> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }
        .map_err(|_| "target_process_unavailable")?;
    Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

impl ForegroundTarget {
    pub(crate) fn output_code(&self, chord: &Chord) -> Result<(u32, u32), String> {
        let keys = crate::windows::chord_keys(chord, HKL(self.layout as *mut _))?;
        let hotkey = crate::hotkey::Hotkey::new(chord.modifiers().iter().copied(), chord.key());
        let modifiers = crate::windows::HotkeyCode::from_hotkey(&hotkey)
            .ok_or("Unsupported chord")?
            .modifiers
            .0
            & !0x4000;
        Ok((modifiers, u32::from(keys[keys.len() - 1] & 0xff)))
    }
    pub(crate) fn capture() -> Result<Self, String> {
        unsafe {
            let window = GetForegroundWindow();
            if window.is_invalid() {
                return Err("foreground_unavailable".into());
            }
            let mut pid = 0;
            let thread = GetWindowThreadProcessId(window, Some(&mut pid));
            if pid == 0 || thread == 0 {
                return Err("foreground_unavailable".into());
            }
            let handle = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                pid,
            )
            .map_err(|_| "target_process_unavailable")?;
            let process = OwnedHandle::from_raw_handle(handle.0);
            let layout = GetKeyboardLayout(thread).0 as isize;
            if layout == 0 {
                return Err("target_keyboard_layout_unavailable".into());
            }
            let target = Self {
                hwnd: window.0 as isize,
                pid,
                thread,
                created: process_created(handle)?,
                layout,
                process,
            };
            target.validate()?;
            Ok(target)
        }
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        unsafe {
            let window = HWND(self.hwnd as *mut _);
            let mut pid = 0;
            let handle = HANDLE(self.process.as_raw_handle());
            if !IsWindow(Some(window)).as_bool()
                || GetForegroundWindow() != window
                || GetWindowThreadProcessId(window, Some(&mut pid)) != self.thread
                || pid != self.pid
                || WaitForSingleObject(handle, 0) != WAIT_TIMEOUT
                || process_created(handle)? != self.created
            {
                return Err("foreground_target_changed".into());
            }
            if GetKeyboardLayout(self.thread).0 as isize != self.layout {
                return Err("target_keyboard_layout_changed".into());
            }
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SequenceReceipt {
    pub(crate) state: &'static str,
    pub(crate) completed_steps: usize,
    /// Accepted sequence events, excluding best-effort cleanup events.
    pub(crate) dispatched_events: usize,
    pub(crate) cleanup_confirmed: bool,
}

#[derive(Debug)]
pub(crate) struct SequenceFailure {
    pub(crate) message: String,
    pub(crate) receipt: SequenceReceipt,
}
impl std::fmt::Display for SequenceFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for SequenceFailure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyEvent {
    key: u16,
    up: bool,
}

trait InputGate {
    fn validate(&self) -> Result<(), String>;
    fn cancelled(&self) -> bool;
    fn user_keys_down(&self) -> bool;
    fn elapsed(&self) -> Duration;
    fn sleep(&mut self, duration: Duration);
    fn send(&mut self, events: &[KeyEvent]) -> usize;
}
struct NativeGate<'a> {
    target: &'a ForegroundTarget,
    cancel: &'a AtomicBool,
    started: Instant,
}
impl InputGate for NativeGate<'_> {
    fn validate(&self) -> Result<(), String> {
        self.target.validate()
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    fn user_keys_down(&self) -> bool {
        (8..=255).any(|key| unsafe { GetAsyncKeyState(key) } < 0)
    }
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
    fn send(&mut self, events: &[KeyEvent]) -> usize {
        let input: Vec<INPUT> = events
            .iter()
            .map(|event| {
                let mut flags = KEYBD_EVENT_FLAGS(0);
                if event.up {
                    flags |= KEYEVENTF_KEYUP;
                }
                if event.key > 0xff
                    || matches!(event.key,0x21..=0x2e|0x5b..=0x5d|0x6f|0x90|0xa6..=0xb7)
                {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VIRTUAL_KEY(event.key & 0xff),
                            dwFlags: flags,
                            ..Default::default()
                        },
                    },
                }
            })
            .collect();
        unsafe { SendInput(&input, std::mem::size_of::<INPUT>() as i32) as usize }
    }
}

fn check(gate: &impl InputGate) -> Result<(), String> {
    if gate.cancelled() {
        return Err("sequence_cancelled".into());
    }
    if gate.elapsed() >= MAX_RUNTIME {
        return Err("sequence_timed_out".into());
    }
    gate.validate()
}
fn failure(message: String, mut receipt: SequenceReceipt) -> SequenceFailure {
    receipt.state = if receipt.dispatched_events == 0 {
        "rejected_before_dispatch"
    } else {
        "dispatch_incomplete"
    };
    SequenceFailure { message, receipt }
}

fn run(
    gate: &mut impl InputGate,
    steps: &[(Vec<u16>, u64)],
) -> Result<SequenceReceipt, SequenceFailure> {
    let mut receipt = SequenceReceipt {
        state: "dispatched_unverified",
        completed_steps: 0,
        dispatched_events: 0,
        cleanup_confirmed: true,
    };
    for (keys, delay) in steps {
        let result = (|| -> Result<(), String> {
            check(gate)?;
            let release_deadline = gate.elapsed() + RELEASE_WAIT;
            while gate.user_keys_down() {
                check(gate)?;
                if gate.elapsed() >= release_deadline {
                    return Err("user_keys_still_held".into());
                }
                gate.sleep(Duration::from_millis(10));
            }
            check(gate)?;
            let mut events: Vec<KeyEvent> = keys
                .iter()
                .map(|key| KeyEvent {
                    key: *key,
                    up: false,
                })
                .collect();
            events.extend(keys.iter().rev().map(|key| KeyEvent {
                key: *key,
                up: true,
            }));
            let accepted = gate.send(&events).min(events.len());
            receipt.dispatched_events += accepted;
            if accepted != events.len() {
                let mut held = Vec::new();
                for event in &events[..accepted] {
                    if event.up {
                        held.retain(|key| *key != event.key);
                    } else {
                        held.push(event.key);
                    }
                }
                let cleanup: Vec<KeyEvent> = held
                    .into_iter()
                    .rev()
                    .map(|key| KeyEvent { key, up: true })
                    .collect();
                receipt.cleanup_confirmed =
                    cleanup.is_empty() || gate.send(&cleanup) == cleanup.len();
                return Err(if receipt.cleanup_confirmed {
                    "input_dispatch_partial"
                } else {
                    "input_dispatch_partial_cleanup_unconfirmed"
                }
                .into());
            }
            receipt.completed_steps += 1;
            check(gate)?;
            let delay_deadline = gate.elapsed() + Duration::from_millis(*delay);
            while gate.elapsed() < delay_deadline {
                check(gate)?;
                gate.sleep((delay_deadline - gate.elapsed()).min(Duration::from_millis(10)));
            }
            check(gate)?;
            Ok(())
        })();
        if let Err(message) = result {
            return Err(failure(message, receipt));
        }
    }
    Ok(receipt)
}

/// Each delay follows its chord; every injected key is released before waiting.
pub(crate) fn execute(
    chords: &[(Chord, u64)],
    target: &ForegroundTarget,
    cancel: &AtomicBool,
) -> Result<SequenceReceipt, SequenceFailure> {
    let empty = SequenceReceipt {
        state: "rejected_before_dispatch",
        completed_steps: 0,
        dispatched_events: 0,
        cleanup_confirmed: true,
    };
    let total = chords
        .iter()
        .try_fold(0u64, |total, (_, delay)| total.checked_add(*delay));
    if chords.is_empty()
        || chords.len() > MAX_STEPS
        || chords.iter().any(|(_, delay)| *delay > MAX_DELAY_MS)
        || total.is_none_or(|total| total > MAX_TOTAL_DELAY_MS)
    {
        return Err(failure("sequence_budget_exceeded".into(), empty));
    }
    let steps = chords
        .iter()
        .map(|(chord, delay)| {
            crate::windows::chord_keys(chord, HKL(target.layout as *mut _))
                .map(|keys| (keys, *delay))
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|message| failure(message, empty))?;
    run(
        &mut NativeGate {
            target,
            cancel,
            started: Instant::now(),
        },
        &steps,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct FakeGate {
        elapsed: Duration,
        cancel_at: Option<Duration>,
        focus_lost: bool,
        lose_focus_after_send: bool,
        held: bool,
        counts: std::collections::VecDeque<usize>,
        sent: Vec<Vec<KeyEvent>>,
    }
    impl InputGate for FakeGate {
        fn validate(&self) -> Result<(), String> {
            if self.focus_lost {
                Err("foreground_target_changed".into())
            } else {
                Ok(())
            }
        }
        fn cancelled(&self) -> bool {
            self.cancel_at.is_some_and(|at| self.elapsed >= at)
        }
        fn user_keys_down(&self) -> bool {
            self.held
        }
        fn elapsed(&self) -> Duration {
            self.elapsed
        }
        fn sleep(&mut self, duration: Duration) {
            self.elapsed += duration;
        }
        fn send(&mut self, events: &[KeyEvent]) -> usize {
            self.sent.push(events.to_vec());
            if self.lose_focus_after_send {
                self.focus_lost = true;
            }
            self.counts.pop_front().unwrap_or(events.len())
        }
    }
    #[test]
    fn partial_write_releases_only_accepted_injected_keys_in_reverse_order() {
        let mut gate = FakeGate {
            counts: [2, 2].into(),
            ..Default::default()
        };
        let error = run(&mut gate, &[(vec![0x11, 0x12, 0x41], 0)]).unwrap_err();
        assert_eq!(error.receipt.dispatched_events, 2);
        assert!(error.receipt.cleanup_confirmed);
        assert_eq!(
            gate.sent[1],
            vec![
                KeyEvent {
                    key: 0x12,
                    up: true
                },
                KeyEvent {
                    key: 0x11,
                    up: true
                }
            ]
        );
    }
    #[test]
    fn failed_cleanup_is_uncertain_and_not_retried_as_another_chord() {
        let mut gate = FakeGate {
            counts: [1, 0].into(),
            ..Default::default()
        };
        let error = run(&mut gate, &[(vec![0x11, 0x41], 0)]).unwrap_err();
        assert!(!error.receipt.cleanup_confirmed);
        assert_eq!(gate.sent.len(), 2);
    }
    #[test]
    fn focus_change_stops_before_next_step_and_cancel_wait_never_releases_user_keys() {
        let mut gate = FakeGate {
            lose_focus_after_send: true,
            ..Default::default()
        };
        let error = run(&mut gate, &[(vec![0x41], 0), (vec![0x42], 0)]).unwrap_err();
        assert_eq!(error.receipt.completed_steps, 1);
        assert_eq!(gate.sent.len(), 1);
        let mut gate = FakeGate {
            held: true,
            cancel_at: Some(Duration::from_millis(20)),
            ..Default::default()
        };
        assert_eq!(
            run(&mut gate, &[(vec![0x41], 0)]).unwrap_err().message,
            "sequence_cancelled"
        );
        assert!(gate.sent.is_empty());
    }
    #[test]
    fn chords_release_before_delays_and_success_is_only_dispatch_evidence() {
        let mut gate = FakeGate::default();
        let result = run(&mut gate, &[(vec![0x11, 0x41], 50), (vec![0x0d], 0)]).unwrap();
        assert_eq!(result.state, "dispatched_unverified");
        assert_eq!(result.completed_steps, 2);
        assert_eq!(
            gate.sent[0].last(),
            Some(&KeyEvent {
                key: 0x11,
                up: true
            })
        );
        assert_eq!(gate.elapsed, Duration::from_millis(50));
    }

    #[test]
    fn held_user_keys_and_cancellation_at_delay_boundary_do_not_dispatch_more() {
        let mut held = FakeGate {
            held: true,
            ..Default::default()
        };
        assert_eq!(
            run(&mut held, &[(vec![0x41], 0)]).unwrap_err().message,
            "user_keys_still_held"
        );
        assert_eq!(held.elapsed, RELEASE_WAIT);
        assert!(held.sent.is_empty());
        let mut cancelled = FakeGate {
            cancel_at: Some(Duration::from_millis(50)),
            ..Default::default()
        };
        let error = run(&mut cancelled, &[(vec![0x41], 50)]).unwrap_err();
        assert_eq!(error.message, "sequence_cancelled");
        assert_eq!(error.receipt.completed_steps, 1);
        assert_eq!(cancelled.sent.len(), 1);
    }
}
