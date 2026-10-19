#![cfg(windows)]

//! Generic Windows HID game-controller input adapter.
//!
//! The adapter enumerates Windows HID top-level collections that identify
//! themselves as a joystick, game pad, or multi-axis controller, keeps their
//! handles open, and reports button press/release transitions. It is poll based:
//! the service owns a [`Monitor`] on its accept-loop thread, calls
//! [`Monitor::refresh`] when it needs the device list, and calls
//! [`Monitor::poll`] every 20-50 ms. No background thread is started.
//!
//! Every method degrades to an empty result when no controller is attached:
//! there are no synthetic devices and no fabricated events.
//!
//! Button usages are normally declared on the HID Button usage page (`0x09`),
//! so the values in `DeviceInfo::button_usages` and `ButtonEvent::usage` are
//! page `0x09` usages for those devices. The Generic Desktop page (`0x01`) is
//! used only when a descriptor declares its button caps there instead; a single
//! device always reports one consistent page. The top-level collection itself
//! must be Generic Desktop page `0x01`, usage `0x04`/`0x05`/`0x08`.

use std::collections::BTreeSet;
use std::ffi::c_void;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetHidGuid, HidD_GetPreparsedData,
    HidD_GetProductString, HidD_GetSerialNumberString, HidP_GetButtonCaps, HidP_GetCaps,
    HidP_GetUsages, HidP_Input, HIDD_ATTRIBUTES, HIDP_BUTTON_CAPS, HIDP_CAPS,
    HIDP_STATUS_USAGE_NOT_FOUND, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_DEVICE_NOT_CONNECTED, ERROR_GEN_FAILURE, ERROR_INVALID_HANDLE,
    ERROR_IO_PENDING, ERROR_NO_SUCH_DEVICE, GENERIC_READ, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

/// Usage page of a Generic Desktop top-level collection.
const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
/// Usage page that HID game controllers normally use for their buttons.
const USAGE_PAGE_BUTTON: u16 = 0x09;
const USAGE_JOYSTICK: u16 = 0x04;
const USAGE_GAME_PAD: u16 = 0x05;
const USAGE_MULTI_AXIS_CONTROLLER: u16 = 0x08;

/// At most this many controllers are opened at once.
const MAX_DEVICES: usize = 8;
/// Bound on enumerated HID interfaces per scan.
const MAX_HID_INTERFACES: usize = 128;
/// Bound on declared button usages per device.
const MAX_BUTTON_USAGES: usize = 128;
/// Sanity bound on input button caps from a malicious or broken descriptor.
const MAX_BUTTON_CAPS: usize = 256;
/// Bound on `device_key` bytes.
const MAX_DEVICE_KEY_BYTES: usize = 512;
/// Bound on `DeviceInfo::name` bytes.
const MAX_DEVICE_NAME_BYTES: usize = 128;
/// Total time `poll` may spend waiting for reports.
const POLL_BUDGET_MS: u32 = 20;
/// Longest single overlapped read wait.
const READ_TIMEOUT_MS: u32 = 10;
/// Total reports processed per `poll` call.
const MAX_REPORTS_PER_POLL: usize = 32;
/// Reports processed per device per `poll` call.
const MAX_REPORTS_PER_DEVICE: usize = 8;

/// A connected HID game controller.
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    /// Stable-enough identity for a saved binding: `vendor:product:serial`
    /// (lowercase hex) when the HID serial number is present, otherwise
    /// `vendor:product:interface-path`. Bounded to 512 printable bytes.
    ///
    /// Identical units that expose no serial number get a path-derived ordinal.
    /// Such bindings may not follow a replaced unit; devices with a serial do.
    pub device_key: String,
    /// Product string, or a readable `HID gamepad vvvv:pppp` fallback.
    pub name: String,
    pub vendor_id: u16,
    pub product_id: u16,
    /// Button usages the device declares, sorted ascending and deduplicated.
    /// Values come from the device's button usage page (normally page `0x09`).
    pub button_usages: Vec<u16>,
    /// Always `true`: `refresh` reports present collections only.
    pub connected: bool,
}

/// One button transition observed in an input report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ButtonEvent {
    pub device_key: String,
    pub usage: u16,
    pub pressed: bool,
}

/// Poll-based gamepad monitor owned by the service accept-loop thread.
#[derive(Default)]
pub struct Monitor {
    /// Open controller collections, bounded by [`MAX_DEVICES`].
    pads: Vec<OpenPad>,
    /// Most recent pressed usage per device key, bounded by [`MAX_DEVICES`].
    latest: Vec<(String, u16, u64)>,
}

// SAFETY: a `Monitor` owns HID file handles and preparsed-data buffers. The
// service hands an idle monitor to its accept-loop thread before polling starts;
// every mutating method takes `&mut self`, so no HID call runs concurrently.
// The handles are plain kernel objects and are not thread affine.
unsafe impl Send for Monitor {}

impl Monitor {
    /// Creates an inert monitor. No devices are opened until `refresh`/`poll`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Rescans HID interfaces, opens the gamepad collections, and returns the
    /// currently connected devices. Call this on the accept-loop thread.
    pub fn refresh(&mut self) -> Vec<DeviceInfo> {
        let paths = match hid_interface_paths() {
            Ok(paths) => paths,
            Err(_) => {
                // An enumeration failure means "no devices"; the caller may retry.
                self.pads.clear();
                self.latest.clear();
                return Vec::new();
            }
        };
        let mut previous = std::mem::take(&mut self.pads);
        let mut pads: Vec<OpenPad> = Vec::new();
        for path in paths {
            if pads.len() >= MAX_DEVICES {
                break;
            }
            let pad = match previous.iter().position(|pad| pad.identity.path == path) {
                // A known interface keeps its open handle and button state.
                Some(index) => previous.remove(index),
                None => match open_pad(&path) {
                    Ok(Some(pad)) => pad,
                    // Not a gamepad collection, or it disappeared mid-scan.
                    Ok(None) | Err(_) => continue,
                },
            };
            pads.push(pad);
        }
        drop(previous);
        let identities: Vec<DeviceIdentity> = pads.iter().map(|pad| pad.identity.clone()).collect();
        for (pad, key) in pads.iter_mut().zip(unique_device_keys(&identities)) {
            pad.key = key;
        }
        self.pads = pads;
        self.latest
            .retain(|(key, _, _)| self.pads.iter().any(|pad| &pad.key == key));
        let mut infos: Vec<DeviceInfo> = self.pads.iter().map(OpenPad::info).collect();
        // Stable ordering for a UI that redraws from this vector.
        infos.sort_by(|left, right| left.device_key.cmp(&right.device_key));
        infos
    }

    /// Reads pending input reports without blocking for long (bounded by
    /// [`POLL_BUDGET_MS`]), updates button state, and returns the transitions.
    pub fn poll(&mut self) -> Vec<ButtonEvent> {
        let mut events: Vec<ButtonEvent> = Vec::new();
        if self.pads.is_empty() {
            return events;
        }
        let deadline = Instant::now() + Duration::from_millis(u64::from(POLL_BUDGET_MS));
        let device_count = self.pads.len().max(1) as u32;
        let per_device_timeout = (POLL_BUDGET_MS / device_count).clamp(1, READ_TIMEOUT_MS);
        let mut reports = 0usize;
        let mut index = 0usize;
        while index < self.pads.len() {
            let mut latest_updates: Vec<(String, u16, u64)> = Vec::new();
            let mut removed = false;
            for attempt in 0..MAX_REPORTS_PER_DEVICE {
                if reports >= MAX_REPORTS_PER_POLL || Instant::now() >= deadline {
                    break;
                }
                let pad = &mut self.pads[index];
                let remaining_ms = deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u32;
                // Only the first read waits; following reads drain reports that
                // are already queued without spending more of the poll budget.
                let timeout = if attempt == 0 {
                    per_device_timeout.min(remaining_ms.max(1))
                } else {
                    0
                };
                match read_report(pad.handle.0, pad.input_len, timeout) {
                    ReadOutcome::Report(mut report) => {
                        reports += 1;
                        let Some(current) =
                            pressed_usages(pad.preparsed.0, pad.button_page, &mut report)
                        else {
                            // A malformed or unrelated report must not be
                            // treated as "all buttons released".
                            continue;
                        };
                        let transitions = diff_button_state(&pad.pressed, &current);
                        if transitions.is_empty() {
                            continue;
                        }
                        let timestamp = unix_ms();
                        for (usage, pressed) in &transitions {
                            if *pressed {
                                latest_updates.push((pad.key.clone(), *usage, timestamp));
                            }
                        }
                        events.extend(button_events(&pad.key, &transitions));
                        pad.pressed = current;
                    }
                    ReadOutcome::Idle | ReadOutcome::Failed => break,
                    ReadOutcome::Removed => {
                        removed = true;
                        break;
                    }
                }
            }
            if removed {
                // Closing the handle happens when the removed pad is dropped.
                let pad = self.pads.remove(index);
                self.latest.retain(|(key, _, _)| key != &pad.key);
            } else {
                index += 1;
            }
            for (key, usage, timestamp) in latest_updates {
                self.record_latest(&key, usage, timestamp);
            }
        }
        events
    }

    /// The most recent pressed button per device, for a UI "press a button to
    /// learn it" flow. Timestamps are milliseconds since the Unix epoch.
    pub fn latest_pressed(&self) -> Vec<(String, u16, u64)> {
        let mut entries: Vec<(String, u16, u64)> = Vec::new();
        for (key, usage, timestamp) in &self.latest {
            match entries.iter().position(|(existing, _, _)| existing == key) {
                Some(position) => {
                    if *timestamp >= entries[position].2 {
                        entries[position] = (key.clone(), *usage, *timestamp);
                    }
                }
                None => entries.push((key.clone(), *usage, *timestamp)),
            }
        }
        // At most one entry per device; keep the output deterministic.
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries.truncate(MAX_DEVICES);
        entries
    }

    fn record_latest(&mut self, device_key: &str, usage: u16, timestamp: u64) {
        if let Some(position) = self.latest.iter().position(|(key, _, _)| key == device_key) {
            self.latest[position] = (device_key.to_string(), usage, timestamp);
            return;
        }
        if self.latest.len() >= MAX_DEVICES {
            self.latest.remove(0);
        }
        self.latest.push((device_key.to_string(), usage, timestamp));
    }
}

/// Identity fields from which a device key is derived.
#[derive(Clone, Debug)]
struct DeviceIdentity {
    vendor_id: u16,
    product_id: u16,
    /// Trimmed serial number; `None` when the device exposes none.
    serial: Option<String>,
    /// Full SetupDi interface path.
    path: String,
}

/// An open gamepad collection.
struct OpenPad {
    /// Assigned by `refresh` once all current identities are known.
    key: String,
    identity: DeviceIdentity,
    name: String,
    handle: OwnedHandle,
    preparsed: PreparsedData,
    /// `HIDP_CAPS::InputReportByteLength`; the read buffer size.
    input_len: usize,
    /// Usage page the declared button usages belong to.
    button_page: Option<u16>,
    button_usages: Vec<u16>,
    /// Button usages pressed in the previous report.
    pressed: BTreeSet<u16>,
}

impl OpenPad {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            device_key: self.key.clone(),
            name: self.name.clone(),
            vendor_id: self.identity.vendor_id,
            product_id: self.identity.product_id,
            button_usages: self.button_usages.clone(),
            connected: true,
        }
    }
}

/// Owns a Win32 handle and closes it on drop.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateFileW and is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Owns the preparsed data allocated by HidD_GetPreparsedData.
struct PreparsedData(PHIDP_PREPARSED_DATA);

impl Drop for PreparsedData {
    fn drop(&mut self) {
        // SAFETY: the pointer came from HidD_GetPreparsedData for this device
        // and is freed exactly once.
        unsafe {
            let _ = HidD_FreePreparsedData(self.0);
        }
    }
}

/// Guards a SetupDi device information set.
struct SetGuard(HDEVINFO);

impl Drop for SetGuard {
    fn drop(&mut self) {
        // SAFETY: the set came from SetupDiGetClassDevsW and is destroyed once.
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested without touching hardware).
// ---------------------------------------------------------------------------

/// True for the top-level collection usages that identify a game controller.
fn is_gamepad_collection(usage_page: u16, usage: u16) -> bool {
    usage_page == USAGE_PAGE_GENERIC_DESKTOP
        && matches!(
            usage,
            USAGE_JOYSTICK | USAGE_GAME_PAD | USAGE_MULTI_AXIS_CONTROLLER
        )
}

/// Usage pages from which input button caps are accepted.
fn is_usable_button_page(usage_page: u16) -> bool {
    usage_page == USAGE_PAGE_BUTTON || usage_page == USAGE_PAGE_GENERIC_DESKTOP
}

/// Chooses the page used for button usages and `HidP_GetUsages`.
///
/// The HID Button page is the standard page for controller buttons; the
/// Generic Desktop page is a fallback for descriptors that declare button caps
/// there. One device never mixes the two pages.
fn preferred_button_page(pages: &[u16]) -> Option<u16> {
    if pages.contains(&USAGE_PAGE_BUTTON) {
        return Some(USAGE_PAGE_BUTTON);
    }
    if pages.contains(&USAGE_PAGE_GENERIC_DESKTOP) {
        return Some(USAGE_PAGE_GENERIC_DESKTOP);
    }
    None
}

/// One declared input-button usage range, normalized to a single page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ButtonCapRange {
    page: u16,
    usage_min: u16,
    usage_max: u16,
}

/// Expands the declared ranges for `page` into sorted, deduplicated usages.
fn collect_usages(ranges: &[ButtonCapRange], page: u16) -> Vec<u16> {
    let mut usages = BTreeSet::new();
    for range in ranges.iter().filter(|range| range.page == page) {
        if range.usage_min == 0 || range.usage_max < range.usage_min {
            continue;
        }
        for usage in range.usage_min..=range.usage_max {
            if usages.len() >= MAX_BUTTON_USAGES {
                break;
            }
            usages.insert(usage);
        }
    }
    usages.into_iter().collect()
}

/// Normalizes usages from one report: drops the undefined usage `0`, removes
/// duplicates, and sorts.
fn normalize_usages(usages: &[u16]) -> BTreeSet<u16> {
    usages.iter().copied().filter(|usage| *usage != 0).collect()
}

/// Returns `(usage, pressed)` transitions between two button sets, presses
/// first. Usages present in both sets produce no transition.
fn diff_button_state(previous: &BTreeSet<u16>, current: &BTreeSet<u16>) -> Vec<(u16, bool)> {
    let mut transitions = Vec::new();
    for usage in current.difference(previous) {
        transitions.push((*usage, true));
    }
    for usage in previous.difference(current) {
        transitions.push((*usage, false));
    }
    transitions
}

fn button_events(device_key: &str, transitions: &[(u16, bool)]) -> Vec<ButtonEvent> {
    transitions
        .iter()
        .map(|(usage, pressed)| ButtonEvent {
            device_key: device_key.to_string(),
            usage: *usage,
            pressed: *pressed,
        })
        .collect()
}

/// Builds the base key for one interface.
///
/// `vendor:product:serial` when a non-empty serial exists, otherwise
/// `vendor:product:path`. Long bodies are truncated and tagged with a hash of
/// the full body so distinct interfaces do not collapse to the same text.
fn format_device_key(identity: &DeviceIdentity) -> String {
    let prefix = format!("{:04x}:{:04x}", identity.vendor_id, identity.product_id);
    let serial = identity
        .serial
        .as_deref()
        .map(str::trim)
        .filter(|serial| !serial.is_empty());
    match serial {
        Some(serial) => compose_bounded_key(&prefix, serial, serial),
        None => compose_bounded_key(&prefix, &identity.path, &identity.path),
    }
}

/// Assigns keys to all current interfaces and keeps them distinct.
///
/// Two devices normally differ by path, so their base keys already differ.
/// When identical units report the same serial (or the same path text), a short
/// ordinal derived from the interface path is appended. A key can therefore
/// change if a duplicate unit is attached or removed; saved bindings for
/// identical devices without serials may not follow a replaced unit.
fn unique_device_keys(identities: &[DeviceIdentity]) -> Vec<String> {
    let bases: Vec<String> = identities.iter().map(format_device_key).collect();
    let mut keys: Vec<String> = Vec::with_capacity(bases.len());
    for (index, base) in bases.iter().enumerate() {
        let duplicated = bases.iter().filter(|other| *other == base).count() > 1;
        if !duplicated {
            keys.push(base.clone());
            continue;
        }
        let suffix = format!("#{:04x}", fnv1a(&identities[index].path) as u16);
        let mut key = append_bounded_suffix(base, &suffix);
        // Two paths can share the low hash bits; the encounter index is a last
        // resort that still keeps this scan's keys distinct.
        if keys.iter().any(|existing| existing == &key) {
            key = append_bounded_suffix(base, &format!("{suffix}-{index}"));
        }
        keys.push(key);
    }
    keys
}

fn compose_bounded_key(prefix: &str, body: &str, hash_source: &str) -> String {
    let body = sanitize_printable(body.trim());
    let plain = format!("{prefix}:{body}");
    if plain.len() <= MAX_DEVICE_KEY_BYTES {
        return plain;
    }
    let suffix = format!("~{:08x}", fnv1a(hash_source) as u32);
    let budget = MAX_DEVICE_KEY_BYTES.saturating_sub(prefix.len() + 1 + suffix.len());
    let mut key = String::with_capacity(MAX_DEVICE_KEY_BYTES);
    key.push_str(prefix);
    key.push(':');
    key.push_str(truncate_utf8(&body, budget));
    key.push_str(&suffix);
    key
}

fn append_bounded_suffix(base: &str, suffix: &str) -> String {
    let keep = MAX_DEVICE_KEY_BYTES.saturating_sub(suffix.len());
    let mut key = truncate_utf8(base, keep).to_string();
    key.push_str(suffix);
    key
}

/// Keeps text printable (no control characters) for keys and UI names.
fn sanitize_printable(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                '_'
            } else {
                character
            }
        })
        .collect()
}

/// Truncates on a UTF-8 boundary so the result is at most `max_bytes` long.
fn truncate_utf8(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// FNV-1a, used only for bounded, printable key suffixes.
fn fnv1a(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn device_name(product: Option<&str>, identity: &DeviceIdentity) -> String {
    let product = product
        .map(sanitize_printable)
        .filter(|product| !product.trim().is_empty());
    let name = match product {
        Some(product) => product.trim().to_string(),
        None => format!(
            "HID gamepad {:04x}:{:04x}",
            identity.vendor_id, identity.product_id
        ),
    };
    truncate_utf8(&name, MAX_DEVICE_NAME_BYTES).to_string()
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// HID enumeration.
// ---------------------------------------------------------------------------

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error(context: &str, error: windows::core::Error) -> String {
    format!("{context}: {error}")
}

/// Enumerates all present HID device interface paths, bounded by
/// [`MAX_HID_INTERFACES`].
fn hid_interface_paths() -> Result<Vec<String>, String> {
    // SAFETY: the SetupDi calls follow the documented two-step detail query; the
    // detail buffer is allocated from the size the API reported, and `set` is
    // released by SetGuard.
    unsafe {
        let guid = HidD_GetHidGuid();
        let set = SetupDiGetClassDevsW(
            Some(&guid),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
        .map_err(|error| last_error("Windows could not enumerate HID interfaces", error))?;
        if set.is_invalid() {
            return Err("Windows returned an invalid HID device set.".to_string());
        }
        let set = SetGuard(set);
        let mut paths = Vec::new();
        let mut index = 0u32;
        loop {
            if paths.len() >= MAX_HID_INTERFACES {
                break;
            }
            let mut interface = SP_DEVICE_INTERFACE_DATA {
                cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDeviceInterfaces(set.0, None, &guid, index, &mut interface).is_err() {
                break;
            }
            index += 1;
            let mut required = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &interface,
                None,
                0,
                Some(&mut required),
                None,
            );
            if required == 0 {
                continue;
            }
            let mut buffer = vec![0u8; required as usize];
            let detail = buffer.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            // `cbSize` is 8 for the x64 ABI; the project's Windows targets are
            // 64-bit only, matching the existing hardware adapter.
            (*detail).cbSize = 8;
            if SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &interface,
                Some(detail),
                required,
                Some(&mut required),
                None,
            )
            .is_err()
            {
                continue;
            }
            let path = PCWSTR::from_raw((*detail).DevicePath.as_ptr())
                .to_string()
                .unwrap_or_default();
            if !path.is_empty() {
                paths.push(path);
            }
        }
        Ok(paths)
    }
}

/// Opens one HID interface read-only and returns it when it is a gamepad
/// collection. `Ok(None)` means "present, but not a game controller".
fn open_pad(path: &str) -> Result<Option<OpenPad>, String> {
    let handle = open_read_only(path)?;
    let mut attributes = HIDD_ATTRIBUTES {
        Size: std::mem::size_of::<HIDD_ATTRIBUTES>() as u32,
        ..Default::default()
    };
    // SAFETY: `handle` is an open HID collection; the call fills the struct.
    if !unsafe { HidD_GetAttributes(handle.0, &mut attributes) } {
        return Err("HID attributes could not be read.".to_string());
    }
    let mut preparsed = PHIDP_PREPARSED_DATA::default();
    // SAFETY: `preparsed` is an out parameter; on success the allocation is
    // owned by PreparsedData and freed on drop.
    if !unsafe { HidD_GetPreparsedData(handle.0, &mut preparsed) } {
        return Err("HID preparsed data could not be read.".to_string());
    }
    let preparsed = PreparsedData(preparsed);
    let mut caps: HIDP_CAPS = unsafe {
        // SAFETY: HIDP_CAPS is a plain C struct with no invalid bit patterns.
        std::mem::zeroed()
    };
    // SAFETY: `preparsed` belongs to this open collection.
    let status = unsafe { HidP_GetCaps(preparsed.0, &mut caps) };
    if status.0 < 0 {
        return Err("HID capabilities could not be read.".to_string());
    }
    if !is_gamepad_collection(caps.UsagePage, caps.Usage) {
        // Keyboard, mouse, consumer-control, and vendor collections are skipped.
        return Ok(None);
    }
    let product = hid_string(handle.0, false);
    let serial = hid_string(handle.0, true);
    let ranges = read_button_cap_ranges(preparsed.0, &caps);
    let pages: Vec<u16> = ranges.iter().map(|range| range.page).collect();
    let button_page = preferred_button_page(&pages);
    let button_usages = button_page
        .map(|page| collect_usages(&ranges, page))
        .unwrap_or_default();
    let input_len = usize::from(caps.InputReportByteLength).max(1);
    let identity = DeviceIdentity {
        vendor_id: attributes.VendorID,
        product_id: attributes.ProductID,
        serial: serial
            .map(|serial| serial.trim().to_string())
            .filter(|serial| !serial.is_empty()),
        path: path.to_string(),
    };
    let name = device_name(product.as_deref(), &identity);
    Ok(Some(OpenPad {
        // Assigned by `refresh`, which can see all current interfaces.
        key: String::new(),
        identity,
        name,
        handle,
        preparsed,
        input_len,
        button_page,
        button_usages,
        pressed: BTreeSet::new(),
    }))
}

/// Opens an interface with shared read-only access and overlapped I/O.
fn open_read_only(path: &str) -> Result<OwnedHandle, String> {
    let path = wide(path);
    // SAFETY: `path` is NUL-terminated; shared read-only access does not disturb
    // the owning game's use of the controller.
    unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
            None,
        )
        .map(OwnedHandle)
        .map_err(|error| last_error("Could not open a HID interface", error))
    }
}

/// Reads the product or serial number string, if the device exposes one.
fn hid_string(handle: HANDLE, serial: bool) -> Option<String> {
    let mut buffer = [0u16; 256];
    let length = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: the buffer is valid for `length` bytes and the handle is an open
    // HID collection; both calls only write into the caller buffer.
    let ok = unsafe {
        if serial {
            HidD_GetSerialNumberString(handle, buffer.as_mut_ptr() as *mut c_void, length)
        } else {
            HidD_GetProductString(handle, buffer.as_mut_ptr() as *mut c_void, length)
        }
    };
    if !ok {
        return None;
    }
    let end = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    if end == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..end]))
}

// ---------------------------------------------------------------------------
// HID parsing.
// ---------------------------------------------------------------------------

/// Reads the device's input button caps and normalizes them into ranges.
///
/// A device whose caps cannot be read keeps an empty button list; usages are
/// never guessed.
fn read_button_cap_ranges(
    preparsed: PHIDP_PREPARSED_DATA,
    caps: &HIDP_CAPS,
) -> Vec<ButtonCapRange> {
    let count = usize::from(caps.NumberInputButtonCaps);
    if count == 0 || count > MAX_BUTTON_CAPS {
        return Vec::new();
    }
    let mut buffer = vec![HIDP_BUTTON_CAPS::default(); count];
    let mut length = count as u16;
    // SAFETY: `buffer` holds `length` caps and `preparsed` belongs to the open
    // collection; the API writes at most `length` caps back into the buffer.
    let status =
        unsafe { HidP_GetButtonCaps(HidP_Input, buffer.as_mut_ptr(), &mut length, preparsed) };
    if status.0 < 0 {
        return Vec::new();
    }
    let length = usize::from(length).min(buffer.len());
    button_cap_ranges(&buffer[..length])
}

fn button_cap_ranges(caps: &[HIDP_BUTTON_CAPS]) -> Vec<ButtonCapRange> {
    let mut ranges = Vec::new();
    for cap in caps {
        if !is_usable_button_page(cap.UsagePage) {
            continue;
        }
        // SAFETY: reading the union is valid because `IsRange` selects the arm
        // the HID parser filled in.
        let range = unsafe {
            if cap.IsRange {
                let range = cap.Anonymous.Range;
                ButtonCapRange {
                    page: cap.UsagePage,
                    usage_min: range.UsageMin,
                    usage_max: range.UsageMax,
                }
            } else {
                let button = cap.Anonymous.NotRange;
                ButtonCapRange {
                    page: cap.UsagePage,
                    usage_min: button.Usage,
                    usage_max: button.Usage,
                }
            }
        };
        ranges.push(range);
    }
    ranges
}

/// Reads the currently pressed top-level button usages from one report.
///
/// Returns `None` when the report cannot be parsed (for example a report for a
/// different report ID); callers must skip such reports rather than treat them
/// as "nothing pressed".
fn pressed_usages(
    preparsed: PHIDP_PREPARSED_DATA,
    page: Option<u16>,
    report: &mut [u8],
) -> Option<BTreeSet<u16>> {
    let page = page?;
    let mut usages = [0u16; MAX_BUTTON_USAGES];
    let mut length = MAX_BUTTON_USAGES as u32;
    // SAFETY: `preparsed` belongs to this device, `report` is one complete
    // input report, and the usage buffer is sized by `length`.
    let status = unsafe {
        HidP_GetUsages(
            HidP_Input,
            page,
            Some(0),
            usages.as_mut_ptr(),
            &mut length,
            preparsed,
            report,
        )
    };
    if status.0 < 0 && status != HIDP_STATUS_USAGE_NOT_FOUND {
        return None;
    }
    let length = (length as usize).min(usages.len());
    Some(normalize_usages(&usages[..length]))
}

// ---------------------------------------------------------------------------
// Overlapped input reads.
// ---------------------------------------------------------------------------

enum ReadOutcome {
    /// One complete input report.
    Report(Vec<u8>),
    /// No report arrived within the timeout.
    Idle,
    /// The device disappeared; the caller must close the handle.
    Removed,
    /// A transient error; keep the device open and retry on a later poll.
    Failed,
}

fn read_report(handle: HANDLE, buffer_len: usize, timeout_ms: u32) -> ReadOutcome {
    let event = match unsafe {
        // SAFETY: a manual-reset event is the documented completion signal for
        // overlapped HID I/O.
        CreateEventW(None, true, false, PCWSTR::null())
    } {
        Ok(event) => OwnedHandle(event),
        Err(_) => return ReadOutcome::Failed,
    };
    let mut overlapped = OVERLAPPED {
        hEvent: event.0,
        ..Default::default()
    };
    let mut buffer = vec![0u8; buffer_len.max(1)];
    let mut read = 0u32;
    // SAFETY: `buffer` stays alive until the operation completes (every branch
    // below waits for or cancels and reaps the operation) and `overlapped`
    // remains valid for the duration of that wait.
    let result = unsafe {
        ReadFile(
            handle,
            Some(&mut buffer),
            Some(&mut read),
            Some(&mut overlapped),
        )
    };
    match result {
        Ok(()) => read_outcome(buffer, read),
        Err(error) if error.code() == ERROR_IO_PENDING.to_hresult() => {
            // SAFETY: the event is owned by this function and is only waited on.
            let wait = unsafe { WaitForSingleObject(event.0, timeout_ms) };
            if wait != WAIT_OBJECT_0 {
                // Timeout (or a failed wait): cancel and reap so the buffers
                // never outlive an in-flight operation.
                let transferred = cancel_and_wait(handle, &overlapped);
                return match transferred {
                    Some(bytes) => read_outcome(buffer, bytes),
                    None if wait == WAIT_TIMEOUT => ReadOutcome::Idle,
                    None => ReadOutcome::Failed,
                };
            }
            let mut transferred = 0u32;
            // SAFETY: the event signalled completion, so this only collects the
            // result of the operation started above.
            match unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, false) } {
                Ok(()) => read_outcome(buffer, transferred),
                Err(error) => classify_read_error(error),
            }
        }
        Err(error) => classify_read_error(error),
    }
}

/// Cancels a pending read and waits until it is reaped. Returns the number of
/// bytes when the read won the race against the cancellation.
fn cancel_and_wait(handle: HANDLE, overlapped: &OVERLAPPED) -> Option<u32> {
    // SAFETY: `overlapped` belongs to an in-flight ReadFile on `handle`; the
    // blocking GetOverlappedResult reaps it before the structure is released.
    unsafe {
        let _ = CancelIoEx(handle, Some(overlapped));
        let mut transferred = 0u32;
        match GetOverlappedResult(handle, overlapped, &mut transferred, true) {
            Ok(()) if transferred > 0 => Some(transferred),
            _ => None,
        }
    }
}

fn read_outcome(mut buffer: Vec<u8>, transferred: u32) -> ReadOutcome {
    let transferred = transferred as usize;
    if transferred == 0 {
        return ReadOutcome::Idle;
    }
    buffer.truncate(transferred.min(buffer.len()));
    ReadOutcome::Report(buffer)
}

fn classify_read_error(error: windows::core::Error) -> ReadOutcome {
    let code = error.code();
    let removed = code == ERROR_DEVICE_NOT_CONNECTED.to_hresult()
        || code == ERROR_INVALID_HANDLE.to_hresult()
        || code == ERROR_GEN_FAILURE.to_hresult()
        || code == ERROR_NO_SUCH_DEVICE.to_hresult();
    if removed {
        ReadOutcome::Removed
    } else {
        ReadOutcome::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(
        vendor_id: u16,
        product_id: u16,
        serial: Option<&str>,
        path: &str,
    ) -> DeviceIdentity {
        DeviceIdentity {
            vendor_id,
            product_id,
            serial: serial.map(str::to_string),
            path: path.to_string(),
        }
    }

    fn usage_set(usages: &[u16]) -> BTreeSet<u16> {
        usages.iter().copied().collect()
    }

    #[test]
    fn public_types_can_cross_threads() {
        fn assert_send<T: Send>() {}
        assert_send::<DeviceInfo>();
        assert_send::<ButtonEvent>();
        // The service may build the monitor before starting its accept loop.
        assert_send::<Monitor>();
    }

    #[test]
    fn gamepad_collection_filter_accepts_only_generic_desktop_game_usages() {
        assert!(is_gamepad_collection(0x01, 0x04)); // joystick
        assert!(is_gamepad_collection(0x01, 0x05)); // game pad
        assert!(is_gamepad_collection(0x01, 0x08)); // multi-axis controller
        assert!(!is_gamepad_collection(0x01, 0x02)); // mouse
        assert!(!is_gamepad_collection(0x01, 0x06)); // keyboard
        assert!(!is_gamepad_collection(0x01, 0x00));
        assert!(!is_gamepad_collection(0x0c, 0x04)); // consumer page
        assert!(!is_gamepad_collection(0xff00, 0x05)); // vendor page
    }

    #[test]
    fn device_key_prefers_trimmed_serial_then_full_path() {
        let serial = identity(
            0x045e,
            0x028e,
            Some("  ABC123  "),
            r"\\?\hid#vid_045e&pid_028e#1",
        );
        assert_eq!(format_device_key(&serial), "045e:028e:ABC123");

        let path_only = identity(0x045e, 0x028e, None, r"\\?\hid#vid_045e&pid_028e#2");
        assert_eq!(
            format_device_key(&path_only),
            r"045e:028e:\\?\hid#vid_045e&pid_028e#2"
        );

        let blank_serial = identity(0x17ef, 0x6182, Some("   "), "hid#vid_17ef&pid_6182#3");
        assert_eq!(
            format_device_key(&blank_serial),
            "17ef:6182:hid#vid_17ef&pid_6182#3"
        );
    }

    #[test]
    fn device_key_is_bounded_and_printable() {
        let long_path = format!("hid#vid_0001&pid_0002#{}\u{7}", "a".repeat(2000));
        let key = format_device_key(&identity(1, 2, None, &long_path));
        assert!(key.len() <= MAX_DEVICE_KEY_BYTES);
        assert!(key.starts_with("0001:0002:"));
        assert!(!key.chars().any(char::is_control));

        let long_serial = format!("\u{1}{}", "s".repeat(700));
        let key = format_device_key(&identity(1, 2, Some(&long_serial), "path"));
        assert!(key.len() <= MAX_DEVICE_KEY_BYTES);
        assert!(!key.chars().any(char::is_control));
        assert!(key.contains('~'));
    }

    #[test]
    fn identical_units_stay_distinct() {
        // Same serial: the base keys collide and receive a path-derived ordinal.
        let first = identity(0x0f0d, 0x00c1, Some("SAME"), r"\\?\hid#unit-a");
        let second = identity(0x0f0d, 0x00c1, Some("SAME"), r"\\?\hid#unit-b");
        let keys = unique_device_keys(&[first, second]);
        assert_ne!(keys[0], keys[1]);
        assert!(keys.iter().all(|key| key.contains('#')));
        assert!(keys.iter().all(|key| key.len() <= MAX_DEVICE_KEY_BYTES));

        // Paths that only differ beyond the truncation point stay distinct too.
        let prefix = "p".repeat(1000);
        let first = identity(9, 8, None, &format!("{prefix}a"));
        let second = identity(9, 8, None, &format!("{prefix}b"));
        let keys = unique_device_keys(&[first, second]);
        assert_ne!(keys[0], keys[1]);
    }

    #[test]
    fn button_page_prefers_the_button_usage_page() {
        assert_eq!(preferred_button_page(&[0x09]), Some(0x09));
        assert_eq!(preferred_button_page(&[0x01, 0x09]), Some(0x09));
        assert_eq!(preferred_button_page(&[0x01]), Some(0x01));
        assert_eq!(preferred_button_page(&[0x0c, 0x0d]), None);
        assert_eq!(preferred_button_page(&[]), None);
    }

    #[test]
    fn button_usage_collection_is_sorted_deduplicated_and_bounded() {
        let ranges = [
            ButtonCapRange {
                page: 0x09,
                usage_min: 3,
                usage_max: 5,
            },
            ButtonCapRange {
                page: 0x09,
                usage_min: 1,
                usage_max: 4,
            },
            ButtonCapRange {
                page: 0x01,
                usage_min: 9,
                usage_max: 9,
            },
        ];
        assert_eq!(collect_usages(&ranges, 0x09), vec![1, 2, 3, 4, 5]);
        assert_eq!(collect_usages(&ranges, 0x01), vec![9]);
        let huge = [ButtonCapRange {
            page: 0x09,
            usage_min: 1,
            usage_max: u16::MAX,
        }];
        assert_eq!(collect_usages(&huge, 0x09).len(), MAX_BUTTON_USAGES);
    }

    #[test]
    fn button_diff_reports_press_and_release_once() {
        let previous = usage_set(&[1, 2]);
        let current = usage_set(&[2, 3]);
        assert_eq!(
            diff_button_state(&previous, &current),
            vec![(3, true), (1, false)]
        );
        // Unchanged state and duplicate usages in a report produce nothing.
        assert!(diff_button_state(&previous, &usage_set(&[1, 1, 2])).is_empty());
        assert!(diff_button_state(&previous, &previous).is_empty());
    }

    #[test]
    fn button_events_carry_the_device_key() {
        let events = button_events("045e:028e:ABC123", &[(3, true), (1, false)]);
        assert_eq!(
            events,
            vec![
                ButtonEvent {
                    device_key: "045e:028e:ABC123".to_string(),
                    usage: 3,
                    pressed: true,
                },
                ButtonEvent {
                    device_key: "045e:028e:ABC123".to_string(),
                    usage: 1,
                    pressed: false,
                },
            ]
        );
        assert!(button_events("key", &[]).is_empty());
    }

    #[test]
    fn latest_pressed_keeps_the_newest_entry_per_device() {
        let mut monitor = Monitor::new();
        monitor.latest.push(("pad-a".to_string(), 1, 10));
        monitor.latest.push(("pad-a".to_string(), 2, 20));
        monitor.latest.push(("pad-b".to_string(), 5, 15));
        assert_eq!(
            monitor.latest_pressed(),
            vec![("pad-a".to_string(), 2, 20), ("pad-b".to_string(), 5, 15),]
        );

        for index in 0..MAX_DEVICES * 3 {
            monitor
                .latest
                .push((format!("pad-{index:02}"), 1, index as u64));
        }
        let latest = monitor.latest_pressed();
        assert!(latest.len() <= MAX_DEVICES);
        let mut keys: Vec<&str> = latest.iter().map(|(key, _, _)| key.as_str()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), latest.len());
    }

    #[test]
    fn monitor_is_inert_without_a_gamepad() {
        let mut monitor = Monitor::new();
        assert!(monitor.poll().is_empty());
        assert!(monitor.latest_pressed().is_empty());
        // Rescanning a host without a controller must not panic. The result
        // depends on the host's HID devices, so only its shape is asserted.
        for device in monitor.refresh() {
            assert!(!device.device_key.is_empty());
            assert!(device.device_key.len() <= MAX_DEVICE_KEY_BYTES);
            assert!(!device.device_key.chars().any(char::is_control));
            assert!(device
                .button_usages
                .windows(2)
                .all(|pair| pair[0] < pair[1]));
        }
        let _ = monitor.latest_pressed();
    }
}
