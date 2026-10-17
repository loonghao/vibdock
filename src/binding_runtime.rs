//! One bounded action worker; hotkey registrations stay on their owning service thread.
use crate::{
    bindings::{Action, Binding, Store},
    service::Config,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};

#[cfg(windows)]
type Registration = crate::windows::RegisteredHotkey;
#[cfg(not(windows))]
type Registration = ();

#[cfg(windows)]
#[derive(Clone)]
struct Plan {
    id: i32,
    binding_id: Option<String>,
    hotkey: crate::hotkey::Hotkey,
    native_code: (u32, u32),
}
#[cfg(windows)]
impl Plan {
    fn new(
        id: i32,
        binding_id: Option<String>,
        hotkey: crate::hotkey::Hotkey,
    ) -> Result<Self, String> {
        let code = crate::windows::HotkeyCode::from_hotkey(&hotkey)
            .ok_or("Trigger is unavailable on this keyboard layout")?;
        Ok(Self {
            id,
            binding_id,
            hotkey,
            native_code: (code.modifiers.0 & !0x4000, code.virtual_key),
        })
    }
}
struct Job {
    binding: Binding,
    cancel: Arc<AtomicBool>,
    #[cfg(windows)]
    target: Option<crate::key_sequence_windows::ForegroundTarget>,
}
pub(crate) struct Runtime {
    sender: Option<mpsc::SyncSender<Job>>,
    receiver: mpsc::Receiver<Value>,
    thread: Option<std::thread::JoinHandle<()>>,
    active: Option<Arc<AtomicBool>>,
    execution: Value,
    rejection: Option<String>,
    activation: Vec<Value>,
    #[cfg(windows)]
    current: Vec<Binding>,
    #[cfg(windows)]
    plans: Vec<Plan>,
    #[cfg(windows)]
    registrations: Vec<Registration>,
    #[cfg(windows)]
    next_id: i32,
}
impl Runtime {
    pub(crate) fn new() -> crate::Result<Self> {
        let (sender, jobs) = mpsc::sync_channel::<Job>(1);
        let (results, receiver) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("binding-actions".into())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    let legacy_device = if job.binding.id == "legacy-microphone" {
                        job.binding.actions.first().and_then(|action| {
                            if let Action::Microphone { device_id, .. } = action {
                                Some(device_id.clone())
                            } else {
                                None
                            }
                        })
                    } else {
                        None
                    };
                    let mut result = run_job(job);
                    if let Some(id) = legacy_device {
                        result["device_id"] = json!(id);
                    }
                    let stopped = result["worker_stopped"] == true;
                    if results.send(result).is_err() || stopped {
                        break;
                    }
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            receiver,
            thread: Some(thread),
            active: None,
            execution: json!({"state":"idle"}),
            rejection: None,
            activation: vec![],
            #[cfg(windows)]
            current: vec![],
            #[cfg(windows)]
            plans: vec![],
            #[cfg(windows)]
            registrations: vec![],
            #[cfg(windows)]
            next_id: 100,
        })
    }
    pub(crate) fn poll(&mut self) -> Option<Value> {
        let result = match self.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Disconnected) if self.active.is_some() => {
                json!({"state":"failed","worker_stopped":true,"error":"Action worker ended without a receipt; outcome unconfirmed"})
            }
            _ => return None,
        };
        // Any queued presses while this worker was occupied are rejected,
        // including injected events racing a cancellation/configuration save.
        #[cfg(windows)]
        let _ = crate::windows::poll_hotkeys();
        self.execution = result.clone();
        self.active = None;
        Some(result)
    }
    pub(crate) fn cancel(&mut self) {
        if let Some(cancel) = &self.active {
            cancel.store(true, Ordering::Relaxed);
        }
    }
    pub(crate) fn view(&self, store: &Store) -> Value {
        let mut value = store.view();
        value["activation"] = json!(self.activation);
        value["execution"] = self.execution.clone();
        value["busy"] = json!(self.active.is_some());
        value["last_rejection"] = json!(self.rejection);
        value
    }
    pub(crate) fn startup(
        &mut self,
        store: &Store,
        legacy: &Config,
        slot: &mut Option<Registration>,
    ) {
        #[cfg(windows)]
        {
            self.current = store.bindings.clone();
        }
        if let Err(error) = self.replace(&store.bindings, legacy, slot, || Ok(())) {
            #[cfg(windows)]
            self.refresh_activation(Some(&error));
            #[cfg(not(windows))]
            let _ = error;
        }
    }
    pub(crate) fn save(
        &mut self,
        store: &mut Store,
        candidate: Vec<Binding>,
        legacy: &Config,
        slot: &mut Option<Registration>,
    ) -> Result<(), String> {
        self.cancel();
        self.replace(&candidate, legacy, slot, || store.commit(candidate.clone()))
    }
    #[cfg(windows)]
    pub(crate) fn trigger(&mut self, id: i32, store: &Store) {
        if self.active.is_some() {
            self.rejection = Some("Binding worker is busy; trigger rejected".into());
            return;
        }
        let Some(plan) = self.plans.iter().find(|p| p.id == id) else {
            return;
        };
        let Some(binding) = store
            .bindings
            .iter()
            .find(|b| Some(&b.id) == plan.binding_id.as_ref() && b.enabled)
            .cloned()
        else {
            return;
        };
        let target = if binding
            .actions
            .iter()
            .any(|a| matches!(a, Action::Keys { .. }))
        {
            match crate::key_sequence_windows::ForegroundTarget::capture() {
                Ok(target) => {
                    let triggers = self
                        .plans
                        .iter()
                        .map(|plan| plan.native_code)
                        .collect::<Vec<_>>();
                    let compatible = binding
                        .actions
                        .iter()
                        .filter_map(|action| {
                            if let Action::Keys { chord, .. } = action {
                                Some(chord)
                            } else {
                                None
                            }
                        })
                        .try_for_each(|chord| match target.output_code(chord) {
                            Ok(code) if !triggers.contains(&code) => Ok(()),
                            Ok(_) => {
                                Err("Keyboard output would retrigger a registered binding"
                                    .to_string())
                            }
                            Err(error) => Err(error),
                        });
                    if let Err(error) = compatible {
                        self.execution = json!({"binding_id":binding.id,"state":"rejected_before_dispatch","error":error});
                        return;
                    }
                    Some(target)
                }
                Err(error) => {
                    self.execution = json!({"binding_id":binding.id,"state":"rejected_before_dispatch","error":error});
                    return;
                }
            }
        } else {
            None
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let id = binding.id.clone();
        match self.sender.as_ref().unwrap().try_send(Job {
            binding,
            cancel: Arc::clone(&cancel),
            target,
        }) {
            Ok(()) => {
                self.active = Some(cancel);
                self.execution = json!({"binding_id":id,"state":"pending","completed_actions":0});
                self.rejection = None;
            }
            Err(_) => {
                self.rejection = Some("Binding worker is unavailable; trigger rejected".into())
            }
        }
    }
    #[cfg(windows)]
    pub(crate) fn trigger_legacy(&mut self, legacy: &Config) {
        if self.active.is_some() {
            self.rejection = Some("Binding worker is busy; legacy trigger rejected".into());
            return;
        }
        let Some(device_id) = legacy.device_id.clone() else {
            return;
        };
        let Ok(Some(trigger)) = legacy.hotkey_binding() else {
            return;
        };
        let binding = Binding {
            id: "legacy-microphone".into(),
            label: "Legacy microphone shortcut".into(),
            enabled: true,
            trigger,
            actions: vec![Action::Microphone {
                device_id,
                operation: crate::bindings::Operation::Toggle,
            }],
        };
        let cancel = Arc::new(AtomicBool::new(false));
        if self
            .sender
            .as_ref()
            .unwrap()
            .try_send(Job {
                binding,
                cancel: Arc::clone(&cancel),
                target: None,
            })
            .is_ok()
        {
            self.active = Some(cancel);
            self.execution =
                json!({"binding_id":"legacy-microphone","state":"pending","completed_actions":0});
        } else {
            self.rejection = Some("Binding worker is unavailable; legacy trigger rejected".into());
        }
    }
    pub(crate) fn validate_legacy(bindings: &[Binding], legacy: &Config) -> Result<(), String> {
        #[cfg(windows)]
        {
            validate_native(bindings, legacy)
        }
        #[cfg(not(windows))]
        {
            let _ = (bindings, legacy);
            Ok(())
        }
    }
    #[cfg(windows)]
    pub(crate) fn legacy_changed(&mut self, legacy: &Config, armed: bool) {
        self.plans.retain(|plan| plan.id != 1);
        if armed {
            if let Ok(Some(hotkey)) = legacy.hotkey_binding() {
                if let Ok(plan) = Plan::new(1, None, hotkey) {
                    self.plans.insert(0, plan);
                }
            }
        }
    }
    fn replace(
        &mut self,
        candidate: &[Binding],
        legacy: &Config,
        slot: &mut Option<Registration>,
        persist: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        Self::validate_legacy(candidate, legacy)?;
        #[cfg(not(windows))]
        {
            let _ = slot;
            persist()?;
            self.activation=candidate.iter().map(|b|json!({"id":b.id,"state":if b.enabled {"unavailable"} else {"disabled"},"error":if b.enabled {Some("Global input requires Windows")} else {None}})).collect();
            Ok(())
        }
        #[cfg(windows)]
        {
            let mut plans = Vec::new();
            if legacy.is_bindable() {
                if let Some(hotkey) = legacy.hotkey_binding()? {
                    plans.push(Plan::new(1, None, hotkey)?);
                }
            }
            for binding in candidate.iter().filter(|b| b.enabled) {
                if self.next_id > 0xbfff {
                    return Err("Hotkey registration IDs exhausted; restart the service".into());
                }
                plans.push(Plan::new(
                    self.next_id,
                    Some(binding.id.clone()),
                    binding.trigger.clone(),
                )?);
                self.next_id += 1;
            }
            let mut old = self.plans.clone();
            old.retain(|p| p.id != 1);
            if legacy.is_bindable() {
                if let Some(hotkey) = legacy.hotkey_binding()? {
                    old.insert(0, Plan::new(1, None, hotkey)?);
                }
            }
            self.registrations.clear();
            drop(slot.take());
            let _ = crate::windows::poll_hotkeys();
            let activate = |plans: &[Plan]| -> Result<Vec<(i32, Registration)>, String> {
                let mut live = Vec::new();
                for plan in plans {
                    live.push((plan.id, Registration::register(&plan.hotkey, plan.id)?));
                }
                Ok(live)
            };
            let mut live = match activate(&plans) {
                Ok(live) => live,
                Err(error) => {
                    self.restore(&old, slot);
                    self.refresh_activation(Some(&error));
                    return Err(error);
                }
            };
            if let Err(error) = persist() {
                live.clear();
                self.restore(&old, slot);
                self.refresh_activation(Some(&error));
                return Err(error);
            }
            for (id, registration) in live {
                if id == 1 {
                    *slot = Some(registration);
                } else {
                    self.registrations.push(registration);
                }
            }
            self.plans = plans;
            self.current = candidate.to_vec();
            self.refresh_activation(None);
            let _ = crate::windows::poll_hotkeys();
            Ok(())
        }
    }
    #[cfg(windows)]
    fn refresh_activation(&mut self, error: Option<&str>) {
        self.activation=self.current.iter().map(|binding| {
            let live=self.plans.iter().any(|plan|plan.binding_id.as_deref()==Some(&binding.id));
            json!({"id":binding.id,"state":if !binding.enabled {"disabled"}else if live {"registered"}else{"failed"},"error":if binding.enabled && !live {Some(error.unwrap_or("Hotkey registration unavailable"))}else{None}})
        }).collect();
    }
    #[cfg(windows)]
    fn restore(&mut self, plans: &[Plan], slot: &mut Option<Registration>) {
        self.plans.clear();
        self.registrations.clear();
        for plan in plans {
            match Registration::register(&plan.hotkey, plan.id) {
                Ok(registration) => {
                    if plan.id == 1 {
                        *slot = Some(registration);
                    } else {
                        self.registrations.push(registration);
                    }
                    self.plans.push(plan.clone());
                }
                Err(error) => {
                    if let Some(id) = &plan.binding_id {
                        self.activation.push(json!({"id":id,"state":"failed","error":format!("Rollback registration failed: {error}")}));
                    }
                }
            }
        }
        let _ = crate::windows::poll_hotkeys();
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.cancel();
        drop(self.sender.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(windows)]
fn validate_native(bindings: &[Binding], legacy: &Config) -> Result<(), String> {
    let code = |hotkey: &crate::hotkey::Hotkey| {
        crate::windows::HotkeyCode::from_hotkey(hotkey)
            .map(|c| (c.modifiers.0 & !0x4000, c.virtual_key))
            .ok_or_else(|| "Trigger is unavailable on this keyboard layout".to_string())
    };
    let mut triggers = Vec::new();
    if legacy.is_bindable() {
        if let Some(hotkey) = legacy.hotkey_binding()? {
            triggers.push(code(&hotkey)?);
        }
    }
    for binding in bindings.iter().filter(|b| b.enabled) {
        let key = code(&binding.trigger)?;
        if triggers.contains(&key) {
            return Err(
                "Binding trigger conflicts with another binding or the legacy microphone shortcut"
                    .into(),
            );
        }
        triggers.push(key);
    }
    for binding in bindings.iter().filter(|b| b.enabled) {
        for action in &binding.actions {
            if let Action::Keys { chord, .. } = action {
                let hotkey =
                    crate::hotkey::Hotkey::new(chord.modifiers().iter().copied(), chord.key());
                if triggers.contains(&code(&hotkey)?) {
                    return Err("Keyboard output would retrigger a registered binding".into());
                }
            }
        }
    }
    Ok(())
}

fn run_job(job: Job) -> Value {
    let mut results = Vec::new();
    #[cfg(windows)]
    let mut worker_stopped = false;
    let mut index = 0;
    while index < job.binding.actions.len() {
        if job.cancel.load(Ordering::Relaxed) {
            return json!({"binding_id":job.binding.id,"state":"cancelled","completed_actions":index,"results":results,"error":"Cancelled; earlier actions are not reversed"});
        }
        let action = &job.binding.actions[index];
        #[cfg(windows)]
        let outcome: Result<Value, String> = match action {
            Action::Microphone {
                device_id,
                operation,
            } => {
                let executable = std::env::current_exe()
                    .map_err(|_| "Native helper executable is unavailable".to_string());
                executable.and_then(|exe| {
                    let (value,cleaned)=crate::resident_apps::run_microphone_action(&exe,&job.cancel,device_id,operation.as_str());
                    if !cleaned {worker_stopped=true;results.push(value);return Err("Microphone helper cleanup is unconfirmed; stop and restart the service before retrying".into());}
                    if value["state"]=="verified" && value["device_id"]==*device_id && value["muted"].is_boolean() && value["error"].is_null() {Ok(value)} else {results.push(value);Err("Microphone action failed or its readback is unconfirmed; earlier audio changes are not reversed".into())}
                })
            }
            Action::Playback {
                entry_id,
                operation,
            } => playback(entry_id, *operation, &job.cancel),
            Action::Keys { .. } => {
                let mut steps = Vec::new();
                for action in &job.binding.actions[index..] {
                    if let Action::Keys { chord, delay_ms } = action {
                        steps.push((chord.clone(), *delay_ms));
                    } else {
                        break;
                    }
                }
                let count = steps.len();
                let result = job.target.as_ref().ok_or_else(||"Key job has no captured foreground".to_string()).and_then(|target|crate::key_sequence_windows::execute(&steps,target,&job.cancel).map_err(|failure| {
                    worker_stopped = !failure.receipt.cleanup_confirmed;
                    index+=failure.receipt.completed_steps;
                    results.push(json!({"state":"failed","completed_steps":failure.receipt.completed_steps,"dispatched_events":failure.receipt.dispatched_events,"cleanup_confirmed":failure.receipt.cleanup_confirmed}));
                    failure.message
                }));
                match result {
                    Ok(receipt) => {
                        index += count - 1;
                        Ok(
                            json!({"state":receipt.state,"completed_steps":receipt.completed_steps,"dispatched_events":receipt.dispatched_events,"cleanup_confirmed":receipt.cleanup_confirmed}),
                        )
                    }
                    Err(error) => Err(error),
                }
            }
        };
        #[cfg(not(windows))]
        let outcome: Result<Value, String> = {
            let _ = action;
            Err("Binding actions require Windows".into())
        };
        #[cfg(not(windows))]
        let worker_stopped = false;
        match outcome {
            Ok(value) => results.push(value),
            Err(error) => {
                return json!({"binding_id":job.binding.id,"state":if job.cancel.load(Ordering::Relaxed){"cancelled"}else{"failed"},"completed_actions":index,"results":results,"worker_stopped":worker_stopped,"error":error})
            }
        }
        index += 1;
    }
    let dispatched = results
        .iter()
        .any(|result| result["state"] == "dispatched_unverified");
    json!({"binding_id":job.binding.id,"state":if dispatched {"dispatched_unverified"} else {"verified"},"completed_actions":index,"results":results,"error":null})
}

#[cfg(windows)]
fn playback(
    entry_id: &str,
    operation: crate::bindings::Operation,
    cancel: &AtomicBool,
) -> Result<Value, String> {
    use crate::service::PlaybackReceiptState;
    use std::time::{Duration, Instant};
    let client = crate::service::ResidentControlClient::discover()
        .map_err(|_| "Playback service unavailable")?;
    let started = Instant::now();
    let view = client
        .summary()
        .map_err(|_| "Playback observation unavailable")?;
    let (reference, pid, desired) = playback_target(&view, entry_id, operation, started.elapsed())?;
    let prepared = client
        .prepare(entry_id, &reference, pid, desired)
        .map_err(|_| "Playback preparation rejected")?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled before playback dispatch".into());
    }
    let _ = client.execute(&prepared);
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        if cancel.load(Ordering::Relaxed) {
            return Err(
                "Playback cancelled after dispatch; audio outcome may be unconfirmed".into(),
            );
        }
        if let Ok(receipt) = client.receipt(prepared.operation_id()) {
            match receipt.state {
                PlaybackReceiptState::Prepared | PlaybackReceiptState::Pending => {}
                PlaybackReceiptState::Verified if receipt.requested_muted == desired => {
                    return Ok(
                        json!({"state":"verified","entry_id":entry_id,"muted":receipt.observed_muted}),
                    )
                }
                _ => return Err(receipt.presentation().into()),
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("Playback outcome unconfirmed; inspect audio before retrying".into())
}

#[cfg(any(windows, test))]
fn playback_target(
    view: &Value,
    entry_id: &str,
    operation: crate::bindings::Operation,
    elapsed: std::time::Duration,
) -> Result<(String, u32, bool), String> {
    use std::time::Duration;
    if view["available"] != true
        || view["age_ms"]
            .as_u64()
            .is_none_or(|age| elapsed + Duration::from_millis(age) >= Duration::from_secs(4))
    {
        return Err("Playback observation expired or unavailable".into());
    }
    let mut rows = view["entries"]
        .as_array()
        .ok_or("Playback entries unavailable")?
        .iter()
        .filter(|row| row["id"] == entry_id);
    let row = rows.next().ok_or("Saved playback entry is unavailable")?;
    if rows.next().is_some() {
        return Err("Saved playback entry is ambiguous".into());
    }
    let sessions = row["sessions"]
        .as_array()
        .filter(|sessions| sessions.len() == 1 && row["complete"] == true)
        .ok_or("Playback binding requires one fresh unique current session")?;
    let session = &sessions[0];
    if session["actions_enabled"] != true {
        return Err("Playback session is not controllable".into());
    }
    let reference = session["session_reference"]
        .as_str()
        .ok_or("Playback selection unavailable")?
        .to_string();
    let pid = session["process_id"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
        .ok_or("Playback process unavailable")?;
    let desired = match operation {
        crate::bindings::Operation::Mute => true,
        crate::bindings::Operation::Unmute => false,
        crate::bindings::Operation::Toggle => !session["muted"]
            .as_bool()
            .ok_or("Playback mute state unknown")?,
    };
    Ok((reference, pid, desired))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(trigger: &str) -> Binding {
        Binding {
            id: "fixture".into(),
            label: "Fixture only".into(),
            enabled: true,
            trigger: crate::hotkey::Hotkey::parse(trigger).unwrap(),
            actions: vec![Action::Keys {
                chord: crate::hotkey::Chord::parse("Enter").unwrap(),
                delay_ms: 0,
            }],
        }
    }
    #[test]
    fn playback_selection_requires_fresh_unique_complete_evidence() {
        use crate::bindings::Operation;
        use std::time::Duration;
        let fixture = json!({"available":true,"age_ms":0,"entries":[{"id":"fixture","complete":true,"sessions":[{"actions_enabled":true,"session_reference":"reference","process_id":123,"muted":false}]}]});
        assert_eq!(
            playback_target(&fixture, "fixture", Operation::Toggle, Duration::ZERO).unwrap(),
            ("reference".into(), 123, true)
        );
        assert!(
            playback_target(&fixture, "fixture", Operation::Mute, Duration::from_secs(4)).is_err()
        );
        for field in ["complete", "sessions"] {
            let mut value = fixture.clone();
            value["entries"][0][field] = json!(false);
            assert!(playback_target(&value, "fixture", Operation::Mute, Duration::ZERO).is_err());
        }
        let mut multiple = fixture.clone();
        multiple["entries"][0]["sessions"]
            .as_array_mut()
            .unwrap()
            .push(fixture["entries"][0]["sessions"][0].clone());
        assert!(playback_target(&multiple, "fixture", Operation::Mute, Duration::ZERO).is_err());
        let mut unknown = fixture;
        unknown["entries"][0]["sessions"][0]["muted"] = Value::Null;
        assert!(playback_target(&unknown, "fixture", Operation::Toggle, Duration::ZERO).is_err());
    }
    #[test]
    fn cancellation_before_first_action_has_no_dispatch() {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut runtime = Runtime::new().unwrap();
        runtime.active = Some(Arc::clone(&cancel));
        runtime.cancel();
        let result = run_job(Job {
            binding: fixture("Ctrl+Alt+F22"),
            cancel,
            #[cfg(windows)]
            target: None,
        });
        assert_eq!(result["state"], "cancelled");
        assert_eq!(result["completed_actions"], 0);
        assert_eq!(result["results"], json!([]));
    }
    #[cfg(windows)]
    #[test]
    fn registration_and_persistence_failures_restore_the_whole_previous_set() {
        std::thread::spawn(|| {
            let legacy = Config {
                device_id: Some("fixture-only".into()),
                hotkey: Some("Ctrl+Shift+F18".into()),
            };
            let old = fixture("Ctrl+Shift+F19");
            let conflict = fixture("Ctrl+Shift+F20");
            let next = fixture("Ctrl+Shift+F21");
            let mut runtime = Runtime::new().unwrap();
            let mut slot = None;
            runtime
                .replace(std::slice::from_ref(&old), &legacy, &mut slot, || Ok(()))
                .unwrap();
            let blocker = Registration::register(&conflict.trigger, 900).unwrap();
            let mut persisted = false;
            assert!(runtime
                .replace(&[conflict], &legacy, &mut slot, || {
                    persisted = true;
                    Ok(())
                })
                .is_err());
            assert!(!persisted);
            assert!(slot.is_some());
            assert_eq!(runtime.current, vec![old.clone()]);
            assert_eq!(runtime.activation[0]["state"], "registered");
            assert!(Registration::register(&old.trigger, 901).is_err());
            assert!(runtime
                .replace(std::slice::from_ref(&next), &legacy, &mut slot, || Err(
                    "fixture disk failure".into()
                ))
                .is_err());
            assert!(slot.is_some());
            assert_eq!(runtime.current, vec![old.clone()]);
            assert!(Registration::register(&old.trigger, 902).is_err());
            assert!(Registration::register(&next.trigger, 903).is_ok());
            drop(blocker);
        })
        .join()
        .unwrap();
    }
    #[test]
    fn native_recursion_includes_legacy_and_numpad_enter_aliases() {
        #[cfg(windows)]
        {
            let legacy = Config {
                device_id: Some("fixture".into()),
                hotkey: Some("Ctrl+Enter".into()),
            };
            let binding = Binding {
                id: "fixture".into(),
                label: "Fixture".into(),
                enabled: true,
                trigger: crate::hotkey::Hotkey::parse("Alt+F9").unwrap(),
                actions: vec![Action::Keys {
                    chord: crate::hotkey::Chord::parse("Ctrl+NumpadEnter").unwrap(),
                    delay_ms: 0,
                }],
            };
            assert!(Runtime::validate_legacy(&[binding], &legacy).is_err());
        }
    }
}
