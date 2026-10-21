//! Opt-in tap/hold input. Only this worker owns the injected held keys.
//!
//! Bare F9..F11 are managed sentinels, not proof of a physical AU05 identity.
//! A dispatched chord does not prove that the input method started recording.

#![cfg_attr(not(windows), allow(dead_code))]

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Tap(u8),
    Started(u8),
    Released(u8),
    Cancelled(u8),
    Failed(u8, String),
}

const FIRST_TRIGGER: u16 = 0x78; // F9; F12 is reserved by Windows.
const MODIFIERS: [u16; 8] = [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C];

fn modifier_family(key: u16) -> &'static [u16] {
    match key {
        0x10 | 0xA0 | 0xA1 => &[0xA0, 0xA1],
        0x11 | 0xA2 | 0xA3 => &[0xA2, 0xA3],
        0x12 | 0xA4 | 0xA5 => &[0xA4, 0xA5],
        0x5B | 0x5C => &[0x5B, 0x5C],
        _ => &[],
    }
}

fn physical_down(physical: &[bool; 256], key: u16) -> bool {
    physical[usize::from(key)]
        || modifier_family(key)
            .iter()
            .any(|key| physical[usize::from(*key)])
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Binding {
    button: u8,
    keys: Vec<u16>,
    long_press_ms: u64,
}

#[derive(Clone, Copy, Debug)]
enum Edge {
    Down(u8, bool, u64),
    Up(u8, u64),
    PhysicalDown(u16),
}

/// The hook only captures edges; SendInput runs outside the hook callback.
struct Capture {
    enabled: [bool; 3],
    captured: [bool; 3],
    physical: [bool; 256],
    edges: Vec<Edge>,
}

impl Capture {
    fn new(physical: [bool; 256]) -> Self {
        Self {
            enabled: [false; 3],
            captured: [false; 3],
            physical,
            edges: Vec::new(),
        }
    }

    fn configure(&mut self, buttons: impl Iterator<Item = u8>) {
        self.enabled = [false; 3];
        for button in buttons {
            self.enabled[usize::from(button)] = true;
        }
        // Never forget a swallowed down: its repeats and up remain ours even
        // when the binding is removed or replaced while the key is held.
    }

    fn edge(&mut self, key: u16, down: bool, now: u64) -> bool {
        let prior = self.physical[usize::from(key)];
        self.physical[usize::from(key)] = down;
        if (FIRST_TRIGGER..FIRST_TRIGGER + 3).contains(&key) {
            let button = (key - FIRST_TRIGGER) as u8;
            let index = usize::from(button);
            if down {
                if !prior {
                    let bare = !MODIFIERS.iter().any(|key| self.physical[usize::from(*key)]);
                    self.captured[index] = self.enabled[index] && bare;
                    self.edges
                        .push(Edge::Down(button, self.captured[index], now));
                }
                return self.captured[index];
            }
            let consumed = self.captured[index];
            self.captured[index] = false;
            if prior {
                self.edges.push(Edge::Up(button, now));
            }
            return consumed;
        }
        if down && !prior {
            self.edges.push(Edge::PhysicalDown(key));
        }
        false
    }
}

trait Input {
    fn is_down(&self, key: u16) -> bool;
    fn send(&mut self, key: u16, up: bool) -> Result<(), String>;
}

#[derive(Default)]
struct HeldKeys(Vec<u16>);

impl HeldKeys {
    fn press(
        &mut self,
        keys: &[u16],
        physical: &[bool; 256],
        input: &mut impl Input,
    ) -> Result<(), String> {
        if !self.0.is_empty() {
            return Err("Previous held keys have not been released.".into());
        }
        // Refuse the whole chord before injecting anything. In particular a
        // pre-held modifier must never become a key owned by this operation.
        if keys
            .iter()
            .any(|key| physical_down(physical, *key) || input.is_down(*key))
        {
            return Err("A voice shortcut key is already held; nothing was pressed.".into());
        }
        for key in keys {
            if let Err(error) = input.send(*key, false) {
                let cleanup = self.release(physical, input);
                return Err(match cleanup {
                    Ok(()) => error,
                    Err(cleanup) => format!("{error} Cleanup failed: {cleanup}"),
                });
            }
            self.0.push(*key);
        }
        Ok(())
    }

    fn release(&mut self, physical: &[bool; 256], input: &mut impl Input) -> Result<(), String> {
        let mut errors = Vec::new();
        for index in (0..self.0.len()).rev() {
            let key = self.0[index];
            // A later physical down takes ownership of its logical key state;
            // the user's physical up will release it. Do not release it early.
            if physical[usize::from(key)] {
                self.0.remove(index);
            } else if let Err(error) = input.send(key, true) {
                errors.push(error);
            } else {
                self.0.remove(index);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
enum Phase {
    #[default]
    Idle,
    Pending(u64),
    Holding,
    Blocked,
}

#[derive(Default)]
struct Button {
    down: bool,
    phase: Phase,
    held: HeldKeys,
}

#[derive(Default)]
struct Machine {
    bindings: Vec<Binding>,
    buttons: [Button; 3],
}

impl Machine {
    fn cancel(&mut self, physical: &[bool; 256], input: &mut impl Input) -> Vec<Event> {
        let mut events = Vec::new();
        for (index, button) in self.buttons.iter_mut().enumerate() {
            if matches!(button.phase, Phase::Pending(_) | Phase::Holding) {
                events.push(Event::Cancelled(index as u8));
            }
            button.phase = if button.down {
                Phase::Blocked
            } else {
                Phase::Idle
            };
            if let Err(error) = button.held.release(physical, input) {
                events.push(Event::Failed(index as u8, error));
            }
        }
        events
    }

    fn configure(
        &mut self,
        bindings: Vec<Binding>,
        physical: &[bool; 256],
        input: &mut impl Input,
    ) -> Vec<Event> {
        let events = self.cancel(physical, input);
        self.bindings = bindings;
        for (index, button) in self.buttons.iter_mut().enumerate() {
            button.down = physical[usize::from(FIRST_TRIGGER) + index];
            button.phase = if button.down {
                Phase::Blocked
            } else {
                Phase::Idle
            };
        }
        events
    }

    fn edge(&mut self, edge: Edge, physical: &[bool; 256], input: &mut impl Input) -> Vec<Event> {
        match edge {
            Edge::Down(index, eligible, now) => {
                let button = &mut self.buttons[usize::from(index)];
                if !button.down {
                    button.down = true;
                    button.phase = if eligible
                        && self.bindings.iter().any(|binding| binding.button == index)
                    {
                        Phase::Pending(now)
                    } else {
                        Phase::Blocked
                    };
                }
                Vec::new()
            }
            Edge::Up(index, now) => {
                let button = &mut self.buttons[usize::from(index)];
                if !button.down {
                    return Vec::new();
                }
                button.down = false;
                let phase = std::mem::take(&mut button.phase);
                match phase {
                    Phase::Pending(start) => {
                        // Re-check elapsed time at release: a delayed timer
                        // must never misclassify an already-long press as a tap.
                        match self.bindings.iter().find(|binding| binding.button == index) {
                            Some(binding) if now.saturating_sub(start) < binding.long_press_ms => {
                                vec![Event::Tap(index)]
                            }
                            _ => vec![Event::Cancelled(index)],
                        }
                    }
                    Phase::Holding => match button.held.release(physical, input) {
                        Ok(()) => vec![Event::Released(index)],
                        Err(error) => vec![Event::Failed(index, error)],
                    },
                    _ => Vec::new(),
                }
            }
            Edge::PhysicalDown(key) => {
                if key == 0x1B
                    || !modifier_family(key).is_empty()
                    || self
                        .buttons
                        .iter()
                        .any(|button| button.held.0.contains(&key))
                {
                    self.cancel(physical, input)
                } else {
                    Vec::new()
                }
            }
        }
    }

    fn tick(&mut self, now: u64, physical: &[bool; 256], input: &mut impl Input) -> Vec<Event> {
        let mut events = Vec::new();
        for binding in &self.bindings {
            let overlapping = self.buttons.iter().enumerate().any(|(index, button)| {
                index != usize::from(binding.button)
                    && button.held.0.iter().any(|key| binding.keys.contains(key))
            });
            let button = &mut self.buttons[usize::from(binding.button)];
            if let Phase::Pending(start) = button.phase {
                if now.saturating_sub(start) >= binding.long_press_ms {
                    button.phase = Phase::Blocked;
                    if overlapping || MODIFIERS.iter().any(|key| physical[usize::from(*key)]) {
                        events.push(Event::Failed(
                            binding.button,
                            "A shortcut key or physical modifier is already held; the voice shortcut was not pressed."
                                .into(),
                        ));
                    } else {
                        match button.held.press(&binding.keys, physical, input) {
                            Ok(()) => {
                                button.phase = Phase::Holding;
                                events.push(Event::Started(binding.button));
                            }
                            Err(error) => events.push(Event::Failed(binding.button, error)),
                        }
                    }
                }
            }
        }
        // Retain ownership after a failed key-up so the next worker cycle can
        // retry cleanup, including after the binding has been removed.
        for button in &mut self.buttons {
            if !matches!(button.phase, Phase::Holding) {
                let _ = button.held.release(physical, input);
            }
        }
        events
    }

    fn released(&self) -> bool {
        self.buttons.iter().all(|button| button.held.0.is_empty())
    }
}

#[cfg(windows)]
pub(crate) use native::Monitor;

#[cfg(windows)]
mod native {
    use super::*;
    use crate::actions::gestures::HoldBinding;
    use crate::hotkey::{Hotkey, Key, Modifier};
    use std::{
        cell::RefCell,
        sync::mpsc::{self, Receiver, Sender},
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };
    use windows::Win32::{
        Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    };

    struct HookState {
        capture: Capture,
        start: Instant,
    }

    thread_local! {
        static HOOK_STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
    }

    unsafe extern "system" fn keyboard_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
        if code >= 0 {
            // The OS owns this pointer for the duration of the hook callback.
            let keyboard = unsafe { &*(l.0 as *const KBDLLHOOKSTRUCT) };
            if keyboard.flags.0 & (LLKHF_INJECTED.0 | LLKHF_LOWER_IL_INJECTED.0) == 0 {
                let down = matches!(w.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
                let up = matches!(w.0 as u32, WM_KEYUP | WM_SYSKEYUP);
                if (down || up) && keyboard.vkCode < 256 {
                    // Normalise generic modifier events to the physical side.
                    let key = match keyboard.vkCode as u16 {
                        0x10 => {
                            if keyboard.scanCode == 0x36 {
                                0xA1
                            } else {
                                0xA0
                            }
                        }
                        0x11 => {
                            if keyboard.flags.0 & LLKHF_EXTENDED.0 != 0 {
                                0xA3
                            } else {
                                0xA2
                            }
                        }
                        0x12 => {
                            if keyboard.flags.0 & LLKHF_EXTENDED.0 != 0 {
                                0xA5
                            } else {
                                0xA4
                            }
                        }
                        key => key,
                    };
                    let consumed = HOOK_STATE.with(|state| {
                        let mut state = state.borrow_mut();
                        state.as_mut().is_some_and(|state| {
                            state
                                .capture
                                .edge(key, down, state.start.elapsed().as_millis() as u64)
                        })
                    });
                    if consumed {
                        return LRESULT(1);
                    }
                }
            }
        }
        unsafe { CallNextHookEx(None, code, w, l) }
    }

    struct Hook(HHOOK);

    impl Drop for Hook {
        fn drop(&mut self) {
            unsafe {
                let _ = UnhookWindowsHookEx(self.0);
            }
            HOOK_STATE.with(|state| *state.borrow_mut() = None);
        }
    }

    #[derive(Default)]
    struct NativeInput;

    impl Input for NativeInput {
        fn is_down(&self, key: u16) -> bool {
            (unsafe { GetAsyncKeyState(i32::from(key)) < 0 })
                || modifier_family(key)
                    .iter()
                    .any(|key| unsafe { GetAsyncKeyState(i32::from(*key)) < 0 })
        }

        fn send(&mut self, key: u16, up: bool) -> Result<(), String> {
            let input = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(key),
                        dwFlags: (if up {
                            KEYEVENTF_KEYUP
                        } else {
                            KEYBD_EVENT_FLAGS::default()
                        }) | if matches!(key, 0x21..=0x2E | 0x5B | 0x5C | 0xA3 | 0xA5) {
                            KEYEVENTF_EXTENDEDKEY
                        } else {
                            KEYBD_EVENT_FLAGS::default()
                        },
                        ..Default::default()
                    },
                },
            };
            if unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) } == 1 {
                Ok(())
            } else {
                Err(format!("Windows rejected a voice shortcut key-{}; check desktop access and application integrity.", if up { "up" } else { "down" }))
            }
        }
    }

    fn compile(binding: &HoldBinding) -> Result<Binding, String> {
        if binding.button > 2 || binding.long_press_ms == 0 {
            return Err("The held shortcut button or threshold is invalid.".into());
        }
        let key = crate::windows::HotkeyCode::from_hotkey(&binding.chord)
            .filter(|code| code.virtual_key > 0 && code.virtual_key < 256)
            .ok_or("The voice shortcut is unavailable on the active keyboard layout.")?
            .virtual_key as u16;
        if (FIRST_TRIGGER..FIRST_TRIGGER + 3).contains(&key) {
            return Err("A voice shortcut cannot contain a managed F9, F10 or F11 trigger.".into());
        }
        let mut keys = binding
            .chord
            .modifiers()
            .iter()
            .map(|modifier| match modifier {
                Modifier::Control => 0xA2,
                Modifier::Alt => 0xA4,
                Modifier::Shift => 0xA0,
                Modifier::Win => 0x5B,
            })
            .collect::<Vec<_>>();
        if let Key::Character(character) = binding.chord.key() {
            if !character.is_ascii_alphabetic() {
                let character = u16::try_from(u32::from(character))
                    .map_err(|_| "Unsupported shortcut character.")?;
                let mapping = unsafe { VkKeyScanW(character) };
                if mapping == -1 {
                    return Err(
                        "The shortcut character is unavailable on this keyboard layout.".into(),
                    );
                }
                for (bit, modifier) in [(1, 0xA0), (2, 0xA2), (4, 0xA4)] {
                    if (mapping as u16 >> 8) & bit != 0 && !keys.contains(&modifier) {
                        keys.push(modifier);
                    }
                }
            }
        }
        if !keys.contains(&key) {
            keys.push(key);
        }
        Ok(Binding {
            button: binding.button,
            keys,
            long_press_ms: binding.long_press_ms,
        })
    }

    enum Command {
        Configure(u64, Vec<HoldBinding>, Sender<Result<(), String>>),
        Stop,
    }

    struct Worker {
        commands: Sender<Command>,
        events: Receiver<(u64, Event)>,
        join: Option<JoinHandle<()>>,
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            let _ = self.commands.send(Command::Stop);
            if let Some(join) = self.join.take() {
                if join.join().is_err() {
                    eprintln!("The held voice input worker stopped unexpectedly.");
                }
            }
        }
    }

    pub(crate) struct Monitor {
        worker: Option<Worker>,
        generation: u64,
        configured: Option<(u64, Vec<HoldBinding>, Result<(), String>)>,
        failure_reported: bool,
    }

    impl Monitor {
        pub(crate) fn new() -> Self {
            Self {
                worker: None,
                generation: 0,
                configured: None,
                failure_reported: false,
            }
        }

        pub(crate) fn configure(
            &mut self,
            revision: u64,
            mut bindings: Vec<HoldBinding>,
        ) -> Result<(), String> {
            bindings.sort_by_key(|binding| binding.button);
            if let Some((old_revision, old_bindings, result)) = &self.configured {
                if *old_revision == revision
                    && *old_bindings == bindings
                    && (result.is_ok() || !bindings.is_empty())
                {
                    return result.clone();
                }
            }
            self.generation = self.generation.wrapping_add(1);
            self.failure_reported = false;
            let result = self.configure_worker(bindings.clone());
            self.configured = Some((revision, bindings, result.clone()));
            result
        }

        fn configure_worker(&mut self, bindings: Vec<HoldBinding>) -> Result<(), String> {
            if self
                .worker
                .as_ref()
                .is_some_and(|worker| worker.join.as_ref().is_some_and(JoinHandle::is_finished))
            {
                self.worker = None;
            }
            if bindings.is_empty() && self.worker.is_none() {
                return Ok(());
            }
            if self.worker.is_none() {
                let (commands, receive) = mpsc::channel();
                let (events, receive_events) = mpsc::channel();
                let join = thread::Builder::new()
                    .name("vibdock-voice-hold".into())
                    .spawn(move || run(receive, events))
                    .map_err(|error| {
                        format!("Cannot start the held voice input worker: {error}")
                    })?;
                self.worker = Some(Worker {
                    commands,
                    events: receive_events,
                    join: Some(join),
                });
            }
            let worker = self.worker.as_ref().expect("worker created above");
            let (reply, receive) = mpsc::channel();
            worker
                .commands
                .send(Command::Configure(self.generation, bindings, reply))
                .map_err(|_| "The held voice input worker is unavailable.".to_string())?;
            receive.recv().map_err(|_| {
                "The held voice input worker could not install its keyboard hook.".to_string()
            })?
        }

        pub(crate) fn poll(&mut self) -> Vec<Event> {
            let mut events = Vec::new();
            let Some(worker) = &self.worker else {
                return events;
            };
            loop {
                match worker.events.try_recv() {
                    Ok((generation, event)) if generation == self.generation => events.push(event),
                    Ok(_) => {}
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if !self.failure_reported {
                            self.failure_reported = true;
                            if let Some((_, bindings, result)) = &mut self.configured {
                                let error = "The held voice input worker stopped; the bindings are disarmed.".to_string();
                                *result = Err(error.clone());
                                events.extend(
                                    bindings.iter().map(|binding| {
                                        Event::Failed(binding.button, error.clone())
                                    }),
                                );
                            }
                        }
                        break;
                    }
                }
            }
            events
        }
    }

    fn snapshot() -> ([bool; 256], Vec<Edge>, u64) {
        HOOK_STATE.with(|state| {
            let mut state = state.borrow_mut();
            let state = state.as_mut().expect("hook state is installed");
            (
                state.capture.physical,
                std::mem::take(&mut state.capture.edges),
                state.start.elapsed().as_millis() as u64,
            )
        })
    }

    fn pump_messages() -> bool {
        let mut message = MSG::default();
        let mut running = true;
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() } {
            if message.message == WM_QUIT {
                running = false;
            } else if message.message != WM_HOTKEY {
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
        running
    }

    #[derive(Default)]
    struct Session {
        machine: Machine,
        input: NativeInput,
    }

    impl Drop for Session {
        fn drop(&mut self) {
            // RAII also unwinds successful downs if the worker panics.
            for _ in 0..40 {
                let _ = pump_messages();
                let _ = self.machine.cancel(&snapshot().0, &mut self.input);
                if self.machine.released() {
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            }
            eprintln!("Windows refused to release held voice shortcut keys; release them physically before continuing.");
        }
    }

    fn run(commands: Receiver<Command>, events: Sender<(u64, Event)>) {
        let mut physical = [false; 256];
        for (key, down) in physical.iter_mut().enumerate() {
            if ![0x10, 0x11, 0x12].contains(&key) {
                *down = unsafe { GetAsyncKeyState(key as i32) < 0 };
            }
        }
        HOOK_STATE.with(|state| {
            *state.borrow_mut() = Some(HookState {
                capture: Capture::new(physical),
                start: Instant::now(),
            })
        });
        let hook = unsafe {
            GetModuleHandleW(None).and_then(|module| {
                SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(keyboard_hook),
                    Some(HINSTANCE(module.0)),
                    0,
                )
            })
        };
        let Ok(hook) = hook else {
            HOOK_STATE.with(|state| *state.borrow_mut() = None);
            return;
        };
        let _hook = Hook(hook);
        let mut session = Session::default();
        let Session { machine, input } = &mut session;
        let mut guards = Vec::new();
        let mut generation = 0;
        let mut stopping = false;
        while !stopping {
            stopping = !pump_messages();
            let (physical, edges, now) = snapshot();
            for edge in edges {
                for event in machine.edge(edge, &physical, input) {
                    let _ = events.send((generation, event));
                }
            }
            loop {
                match commands.try_recv() {
                    Ok(Command::Configure(next_generation, bindings, reply)) => {
                        generation = next_generation;
                        for event in machine.configure(Vec::new(), &physical, input) {
                            let _ = events.send((generation, event));
                        }
                        HOOK_STATE.with(|state| {
                            state
                                .borrow_mut()
                                .as_mut()
                                .expect("hook installed")
                                .capture
                                .configure(std::iter::empty())
                        });
                        guards.clear();
                        let result = (|| {
                            if !machine.released() {
                                return Err("The previous voice shortcut could not be released; new bindings remain disarmed.".into());
                            }
                            let mut compiled = Vec::new();
                            for binding in &bindings {
                                if compiled
                                    .iter()
                                    .any(|old: &Binding| old.button == binding.button)
                                {
                                    return Err(
                                        "A held shortcut button is configured twice.".into()
                                    );
                                }
                                compiled.push(compile(binding)?);
                            }
                            let mut next_guards = Vec::new();
                            for binding in &compiled {
                                let hotkey = Hotkey::new([], Key::Function(9 + binding.button));
                                next_guards.push(crate::windows::RegisteredHotkey::register(
                                    &hotkey,
                                    0x5A00 + i32::from(binding.button),
                                )?);
                            }
                            for event in machine.configure(compiled, &physical, input) {
                                let _ = events.send((generation, event));
                            }
                            guards = next_guards;
                            HOOK_STATE.with(|state| {
                                state
                                    .borrow_mut()
                                    .as_mut()
                                    .expect("hook installed")
                                    .capture
                                    .configure(bindings.iter().map(|binding| binding.button))
                            });
                            Ok(())
                        })();
                        let _ = reply.send(result);
                    }
                    Ok(Command::Stop) | Err(mpsc::TryRecvError::Disconnected) => {
                        stopping = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            if !stopping {
                for event in machine.tick(now, &physical, input) {
                    let _ = events.send((generation, event));
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
        for event in machine.cancel(&snapshot().0, input) {
            let _ = events.send((generation, event));
        }
        drop(guards);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn binding() -> HoldBinding {
            HoldBinding {
                button: 0,
                chord: Hotkey::parse("Ctrl + Win").unwrap(),
                long_press_ms: 550,
            }
        }

        #[test]
        fn unchanged_snapshot_does_not_start_or_reset_a_worker() {
            let mut monitor = Monitor::new();
            monitor.configured = Some((3, vec![binding()], Ok(())));
            assert!(monitor.configure(3, vec![binding()]).is_ok());
            assert!(monitor.worker.is_none());
            assert_eq!(monitor.generation, 0);
        }

        #[test]
        fn queued_old_taps_are_filtered_and_worker_failure_is_reported_once() {
            let (commands, _receive_commands) = mpsc::channel();
            let (send_events, events) = mpsc::channel();
            let mut monitor = Monitor {
                worker: Some(Worker {
                    commands,
                    events,
                    join: None,
                }),
                generation: 2,
                configured: Some((3, vec![binding()], Ok(()))),
                failure_reported: false,
            };
            send_events.send((1, Event::Tap(0))).unwrap();
            send_events.send((2, Event::Released(0))).unwrap();
            assert_eq!(monitor.poll(), vec![Event::Released(0)]);
            drop(send_events);
            assert!(matches!(monitor.poll().as_slice(), [Event::Failed(0, _)]));
            assert!(monitor.poll().is_empty());
            assert!(monitor.configured.as_ref().unwrap().2.is_err());
        }

        #[test]
        fn chord_compilation_rejects_every_managed_trigger() {
            assert_eq!(compile(&binding()).unwrap().keys, vec![0xA2, 0x5B]);
            for number in 9..=11 {
                let mut binding = binding();
                binding.chord = Hotkey::new([Modifier::Control], Key::Function(number));
                assert!(compile(&binding).is_err());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[derive(Default)]
    struct MockInput {
        down: BTreeSet<u16>,
        sent: Vec<(u16, bool)>,
        fail: Option<(u16, bool)>,
    }

    impl Input for MockInput {
        fn is_down(&self, key: u16) -> bool {
            self.down.contains(&key)
        }
        fn send(&mut self, key: u16, up: bool) -> Result<(), String> {
            self.sent.push((key, up));
            if self.fail == Some((key, up)) {
                return Err("rejected".into());
            }
            if up {
                self.down.remove(&key);
            } else {
                self.down.insert(key);
            }
            Ok(())
        }
    }

    fn binding() -> Binding {
        Binding {
            button: 0,
            keys: vec![0xA2, 0x5B],
            long_press_ms: 550,
        }
    }

    fn ready() -> (Machine, MockInput, [bool; 256]) {
        let mut machine = Machine::default();
        let mut input = MockInput::default();
        let physical = [false; 256];
        assert!(machine
            .configure(vec![binding()], &physical, &mut input)
            .is_empty());
        (machine, input, physical)
    }

    #[test]
    fn short_release_is_immediate_and_two_rapid_taps_stay_separate() {
        let (mut machine, mut input, physical) = ready();
        for time in [0, 90] {
            assert!(machine
                .edge(Edge::Down(0, true, time), &physical, &mut input)
                .is_empty());
            assert_eq!(
                machine.edge(Edge::Up(0, time + 40), &physical, &mut input),
                vec![Event::Tap(0)]
            );
        }
        assert!(input.sent.is_empty());
    }

    #[test]
    fn held_threshold_and_repeated_down_never_emit_a_tap() {
        let (mut machine, mut input, physical) = ready();
        machine.edge(Edge::Down(0, true, 10), &physical, &mut input);
        machine.edge(Edge::Down(0, true, 300), &physical, &mut input);
        assert!(machine.tick(559, &physical, &mut input).is_empty());
        assert_eq!(
            machine.tick(560, &physical, &mut input),
            vec![Event::Started(0)]
        );
        assert!(machine.tick(600, &physical, &mut input).is_empty());
        assert_eq!(
            machine.edge(Edge::Up(0, 700), &physical, &mut input),
            vec![Event::Released(0)]
        );
        assert_eq!(
            input.sent,
            vec![(0xA2, false), (0x5B, false), (0x5B, true), (0xA2, true)]
        );
    }

    #[test]
    fn missed_timer_at_or_after_threshold_is_not_a_short_press() {
        for release in [550, 900] {
            let (mut machine, mut input, physical) = ready();
            machine.edge(Edge::Down(0, true, 0), &physical, &mut input);
            assert_eq!(
                machine.edge(Edge::Up(0, release), &physical, &mut input),
                vec![Event::Cancelled(0)]
            );
            assert!(input.sent.is_empty());
        }
    }

    #[test]
    fn escape_and_configuration_change_block_until_real_release() {
        for reconfigure in [false, true] {
            let (mut machine, mut input, mut physical) = ready();
            physical[usize::from(FIRST_TRIGGER)] = true;
            machine.edge(Edge::Down(0, true, 0), &physical, &mut input);
            machine.tick(550, &physical, &mut input);
            let events = if reconfigure {
                machine.configure(Vec::new(), &physical, &mut input)
            } else {
                machine.edge(Edge::PhysicalDown(0x1B), &physical, &mut input)
            };
            assert_eq!(events, vec![Event::Cancelled(0)]);
            assert!(input.down.is_empty());
            if reconfigure {
                machine.configure(vec![binding()], &physical, &mut input);
            }
            machine.edge(Edge::Down(0, true, 800), &physical, &mut input);
            assert!(machine.tick(2000, &physical, &mut input).is_empty());
            physical[usize::from(FIRST_TRIGGER)] = false;
            assert!(machine
                .edge(Edge::Up(0, 2100), &physical, &mut input)
                .is_empty());
            machine.edge(Edge::Down(0, true, 2200), &physical, &mut input);
            assert_eq!(
                machine.edge(Edge::Up(0, 2240), &physical, &mut input),
                vec![Event::Tap(0)]
            );
        }
    }

    #[test]
    fn cancelling_a_pending_tap_never_runs_its_single_action() {
        let (mut machine, mut input, physical) = ready();
        machine.edge(Edge::Down(0, true, 0), &physical, &mut input);
        assert_eq!(
            machine.edge(Edge::PhysicalDown(0x1B), &physical, &mut input),
            vec![Event::Cancelled(0)]
        );
        assert!(machine
            .edge(Edge::Up(0, 50), &physical, &mut input)
            .is_empty());
        assert!(input.sent.is_empty());
    }

    #[test]
    fn partial_down_failure_unwinds_only_successful_downs_in_reverse() {
        let mut held = HeldKeys::default();
        let mut input = MockInput {
            fail: Some((0x20, false)),
            ..Default::default()
        };
        assert!(held
            .press(&[0xA2, 0x5B, 0x20], &[false; 256], &mut input)
            .is_err());
        assert_eq!(
            input.sent,
            vec![
                (0xA2, false),
                (0x5B, false),
                (0x20, false),
                (0x5B, true),
                (0xA2, true)
            ]
        );
        assert!(held.0.is_empty());
        assert!(input.down.is_empty());
    }

    #[test]
    fn preheld_key_refuses_entire_chord_and_failed_hold_does_not_tap() {
        let (mut machine, mut input, physical) = ready();
        input.down.insert(0x5B);
        machine.edge(Edge::Down(0, true, 0), &physical, &mut input);
        assert!(matches!(
            machine.tick(550, &physical, &mut input).as_slice(),
            [Event::Failed(0, _)]
        ));
        assert!(machine
            .edge(Edge::Up(0, 600), &physical, &mut input)
            .is_empty());
        assert!(input.sent.is_empty());
        assert_eq!(input.down, BTreeSet::from([0x5B]));
    }

    #[test]
    fn later_physical_modifier_is_not_released_by_cleanup() {
        let (mut machine, mut input, mut physical) = ready();
        machine.edge(Edge::Down(0, true, 0), &physical, &mut input);
        machine.tick(550, &physical, &mut input);
        physical[0xA2] = true;
        assert_eq!(
            machine.edge(Edge::PhysicalDown(0xA2), &physical, &mut input),
            vec![Event::Cancelled(0)]
        );
        assert_eq!(input.sent, vec![(0xA2, false), (0x5B, false), (0x5B, true)]);
        assert!(machine.released());
        assert!(input.down.contains(&0xA2));
    }

    #[test]
    fn opposite_side_modifier_does_not_strand_the_injected_modifier() {
        let (mut machine, mut input, mut physical) = ready();
        machine.edge(Edge::Down(0, true, 0), &physical, &mut input);
        machine.tick(550, &physical, &mut input);
        physical[0xA3] = true;
        input.down.insert(0xA3);
        assert_eq!(
            machine.edge(Edge::PhysicalDown(0xA3), &physical, &mut input),
            vec![Event::Cancelled(0)]
        );
        assert_eq!(input.down, BTreeSet::from([0xA3]));
        assert!(machine.released());
    }

    #[test]
    fn simultaneous_holds_do_not_share_key_ownership() {
        let (mut machine, mut input, physical) = ready();
        let mut second = binding();
        second.button = 1;
        machine.configure(vec![binding(), second], &physical, &mut input);
        machine.edge(Edge::Down(0, true, 0), &physical, &mut input);
        machine.edge(Edge::Down(1, true, 0), &physical, &mut input);
        assert!(matches!(
            machine.tick(550, &physical, &mut input).as_slice(),
            [Event::Started(0), Event::Failed(1, _)]
        ));
        assert!(machine
            .edge(Edge::Up(1, 600), &physical, &mut input)
            .is_empty());
        assert_eq!(input.sent, vec![(0xA2, false), (0x5B, false)]);
        assert_eq!(
            machine.edge(Edge::Up(0, 700), &physical, &mut input),
            vec![Event::Released(0)]
        );
    }

    #[test]
    fn failed_up_retains_ownership_and_retries_after_binding_removal() {
        let (mut machine, mut input, physical) = ready();
        machine.edge(Edge::Down(0, true, 0), &physical, &mut input);
        machine.tick(550, &physical, &mut input);
        input.fail = Some((0x5B, true));
        assert!(matches!(
            machine
                .configure(Vec::new(), &physical, &mut input)
                .as_slice(),
            [Event::Cancelled(0), Event::Failed(0, _)]
        ));
        assert!(!machine.released());
        input.fail = None;
        machine.tick(600, &physical, &mut input);
        assert!(machine.released());
        assert!(input.down.is_empty());
    }

    #[test]
    fn capture_swallows_repeat_and_up_across_disable_reenable() {
        let mut capture = Capture::new([false; 256]);
        capture.configure([0].into_iter());
        assert!(capture.edge(FIRST_TRIGGER, true, 0));
        assert!(capture.edge(FIRST_TRIGGER, true, 20));
        assert_eq!(capture.edges.len(), 1);
        capture.configure(std::iter::empty());
        assert!(capture.edge(FIRST_TRIGGER, true, 30));
        capture.configure([0].into_iter());
        assert!(capture.edge(FIRST_TRIGGER, false, 40));
        assert_eq!(capture.edges.len(), 2);
        assert!(capture.edge(FIRST_TRIGGER, true, 50));
    }

    #[test]
    fn initial_modifier_or_preexisting_down_never_claims_a_trigger() {
        let mut physical = [false; 256];
        physical[0xA3] = true;
        physical[usize::from(FIRST_TRIGGER) + 1] = true;
        let mut capture = Capture::new(physical);
        capture.configure([0, 1].into_iter());
        assert!(!capture.edge(FIRST_TRIGGER, true, 0));
        assert!(!capture.edge(FIRST_TRIGGER, false, 10));
        assert!(!capture.edge(FIRST_TRIGGER + 1, true, 20));
        assert!(!capture.edge(FIRST_TRIGGER + 1, false, 30));
    }
}
