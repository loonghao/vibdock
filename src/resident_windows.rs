//! Read-only playback observations for the disposable resident-app helper.
//!
//! No saved/client path is accepted here. Paths are correlation locations, not
//! trusted application identities. The parent owns the helper's deadline and
//! kills/reaps that one child on cancellation: COM and filesystem calls cannot
//! be safely cancelled by abandoning an in-process thread.

use std::{collections::BTreeSet, ffi::c_void, ptr};

use serde_json::{json, Value};
use windows::{
    core::{Interface, PCWSTR, PWSTR},
    Win32::{
        Media::Audio::{
            eRender, AudioSessionStateActive, AudioSessionStateInactive, IAudioSessionControl,
            IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume,
            MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
        },
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
            COINIT_APARTMENTTHREADED,
        },
    },
};

const MAX_ENDPOINTS: usize = 8;
const MAX_SESSIONS: usize = 24;
const MAX_ID: usize = 1024;
const MAX_PATH: usize = 4096;
const MAX_ICON: usize = 8192;
const MAX_OUTPUT: usize = 512 * 1024;
const MAX_IMAGE: usize = 256 * 1024 * 1024;
const MAX_RESOURCE: usize = 1024 * 1024;
const MAX_GROUPS: usize = 4;
const MAX_ICON_ENTRIES: usize = 16;

type RawHandle = *mut c_void;

// Kept local to this Windows adapter, avoiding any widening of process access
// or optional Cargo features. All native opens below are read-only.
#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> RawHandle;
    fn CloseHandle(handle: RawHandle) -> i32;
    fn GetProcessId(handle: RawHandle) -> u32;
    fn WaitForSingleObject(handle: RawHandle, milliseconds: u32) -> u32;
    fn GetProcessTimes(
        handle: RawHandle,
        created: *mut [u32; 2],
        exited: *mut [u32; 2],
        kernel: *mut [u32; 2],
        user: *mut [u32; 2],
    ) -> i32;
    fn QueryFullProcessImageNameW(
        handle: RawHandle,
        flags: u32,
        name: *mut u16,
        length: *mut u32,
    ) -> i32;
    fn QueryDosDeviceW(name: *const u16, target: *mut u16, length: u32) -> u32;
    fn GetDriveTypeW(root: *const u16) -> u32;
    fn GetFinalPathNameByHandleW(handle: RawHandle, path: *mut u16, length: u32, flags: u32)
        -> u32;
    fn GetFileInformationByHandle(handle: RawHandle, info: *mut [u32; 13]) -> i32;
    fn CreateFileMappingW(
        file: RawHandle,
        attributes: *const c_void,
        protect: u32,
        high: u32,
        low: u32,
        name: *const u16,
    ) -> RawHandle;
    fn MapViewOfFile(
        mapping: RawHandle,
        access: u32,
        high: u32,
        low: u32,
        bytes: usize,
    ) -> *mut c_void;
    fn UnmapViewOfFile(address: *const c_void) -> i32;
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: RawHandle,
    object_name: *mut UnicodeString,
    attributes: u32,
    security_descriptor: RawHandle,
    security_quality_of_service: RawHandle,
}

#[repr(C)]
struct IoStatusBlock {
    status_or_pointer: usize,
    information: usize,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtOpenFile(
        handle: *mut RawHandle,
        access: u32,
        attributes: *mut ObjectAttributes,
        status: *mut IoStatusBlock,
        share: u32,
        options: u32,
    ) -> i32;
}

struct Handle(RawHandle);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

struct MappedView(*const c_void);
impl Drop for MappedView {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.0);
        }
    }
}

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn bounded_text(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn unavailable(error: &str) -> Value {
    json!({"available":false,"complete":false,"endpoints":[],"sessions":[],
        "skipped":0,"error":error})
}

/// Only call in the owned, short-lived helper process, never an HTTP/UI thread.
pub(crate) fn snapshot() -> Value {
    unsafe { observe_snapshot() }
}

unsafe fn observe_snapshot() -> Value {
    if CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_err() {
        return unavailable("Windows COM could not be initialized for playback observation.");
    }
    let _com = Com;
    let manager: IMMDeviceEnumerator = match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
    {
        Ok(manager) => manager,
        Err(_) => return unavailable("Windows playback observation is unavailable."),
    };
    let (endpoints, omitted) =
        match crate::app_session_windows::render_endpoints_bounded(MAX_ENDPOINTS) {
            Ok(found) => found,
            Err(_) => return unavailable("Active playback endpoints could not be read."),
        };
    let mut skipped = omitted;
    let mut endpoint_values = Vec::new();
    let mut session_values = Vec::new();
    let mut complete = omitted == 0;
    let mut seen_endpoints = BTreeSet::new();
    for (_, endpoint_id) in endpoints {
        if !bounded_text(&endpoint_id, MAX_ID) || !seen_endpoints.insert(endpoint_id.clone()) {
            complete = false;
            skipped += 1;
            continue;
        }
        let budget = MAX_SESSIONS.saturating_sub(session_values.len());
        let mut endpoint_complete = true;
        match crate::app_session_windows::list_bounded(&manager, &endpoint_id, budget) {
            Ok((sessions, unreadable, truncated)) => {
                let initial_count = sessions.len() + unreadable.len() + truncated;
                skipped += unreadable.len() + truncated;
                endpoint_complete &= unreadable.is_empty() && truncated == 0;
                let mut expected = BTreeSet::new();
                for session in sessions {
                    let id = &session.identity.session_instance_id;
                    let pid = session.identity.process_id;
                    if !bounded_text(id, MAX_ID) || !expected.insert((id.clone(), pid)) {
                        skipped += 1;
                        endpoint_complete = false;
                        continue;
                    }
                    match observe_session(&manager, &endpoint_id, id, pid) {
                        Some(observed) => session_values.push(observed),
                        None => {
                            skipped += 1;
                            endpoint_complete = false;
                        }
                    }
                }
                // A changed/incomplete endpoint cannot prove a saved app absent.
                // This final pass also catches additions while icons were read.
                match current_sessions(&manager, &endpoint_id) {
                    Some(current) => {
                        let identities: BTreeSet<_> = current
                            .iter()
                            .map(|(id, pid, _)| (id.clone(), *pid))
                            .collect();
                        endpoint_complete &=
                            current.len() == initial_count && identities == expected;
                    }
                    None => endpoint_complete = false,
                }
            }
            Err(_) => {
                endpoint_complete = false;
                skipped += 1;
            }
        }
        complete &= endpoint_complete;
        endpoint_values.push(json!({"id":endpoint_id,"complete":endpoint_complete}));
    }
    // Detect endpoint churn without turning a failed read into an empty result.
    match crate::app_session_windows::render_endpoints_bounded(MAX_ENDPOINTS) {
        Ok((current, extra)) => {
            let current: BTreeSet<_> = current.into_iter().map(|(_, id)| id).collect();
            if extra != 0 || current != seen_endpoints {
                complete = false;
                for endpoint in &mut endpoint_values {
                    endpoint["complete"] = json!(false);
                }
            }
        }
        Err(_) => {
            complete = false;
            for endpoint in &mut endpoint_values {
                endpoint["complete"] = json!(false);
            }
        }
    }
    let result = json!({
        "available":true, "complete":complete, "endpoints":endpoint_values,
        "sessions":session_values, "skipped":skipped,
        "error":if complete { None } else { Some("Playback observation is partial; absence is unconfirmed.") }
    });
    // Independent final guard, including JSON escaping and all metadata.
    if serde_json::to_vec(&result).is_ok_and(|bytes| bytes.len() < MAX_OUTPUT) {
        result
    } else {
        unavailable("Playback observation exceeded its response budget.")
    }
}

unsafe fn co_string(raw: PWSTR) -> Option<String> {
    if raw.is_null() {
        return None;
    }
    let mut length = 0;
    while length <= MAX_ID && *raw.0.add(length) != 0 {
        length += 1;
    }
    let result = if length <= MAX_ID {
        String::from_utf16(std::slice::from_raw_parts(raw.0, length))
            .ok()
            .filter(|value| bounded_text(value, MAX_ID))
    } else {
        None
    };
    CoTaskMemFree(Some(raw.0.cast()));
    result
}

/// S_OK is essential: the projected Result<u32> also accepts the *successful*
/// AUDCLNT_S_NO_SINGLE_PROCESS warning and would misidentify the initial creator.
unsafe fn single_process(control: &IAudioSessionControl2) -> Option<u32> {
    let mut pid = 0;
    let status = (control.vtable().GetProcessId)(control.as_raw(), &mut pid);
    let system = (control.vtable().IsSystemSoundsSession)(control.as_raw());
    (status.0 == 0 && system.0 == 1 && pid != 0).then_some(pid)
}

unsafe fn current_sessions(
    manager: &IMMDeviceEnumerator,
    endpoint_id: &str,
) -> Option<Vec<(String, u32, IAudioSessionControl2)>> {
    let endpoints = manager
        .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
        .ok()?;
    let count = endpoints.GetCount().ok()? as usize;
    let mut matches = 0;
    for index in 0..count.min(MAX_ENDPOINTS) {
        let device = endpoints.Item(index as u32).ok()?;
        if co_string(device.GetId().ok()?)?.as_str() == endpoint_id {
            matches += 1;
        }
    }
    if matches != 1 {
        return None;
    }
    let id = wide(endpoint_id);
    let device = manager.GetDevice(PCWSTR(id.as_ptr())).ok()?;
    if device.GetState().ok()? != DEVICE_STATE_ACTIVE {
        return None;
    }
    let audio: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None).ok()?;
    let enumerator = audio.GetSessionEnumerator().ok()?;
    let count = usize::try_from(enumerator.GetCount().ok()?).ok()?;
    if count > MAX_SESSIONS {
        return None;
    }
    let mut found = Vec::with_capacity(count);
    let mut seen = BTreeSet::new();
    for index in 0..count {
        let control: IAudioSessionControl = enumerator.GetSession(index as i32).ok()?;
        let control: IAudioSessionControl2 = control.cast().ok()?;
        let id = co_string(control.GetSessionInstanceIdentifier().ok()?)?;
        if !seen.insert(id.clone()) {
            return None;
        }
        // Unknown, multi-process and system sessions are retained as PID 0
        // solely for completeness comparison, never as selectable sessions.
        found.push((id, single_process(&control).unwrap_or(0), control));
    }
    Some(found)
}

unsafe fn verified_control(
    manager: &IMMDeviceEnumerator,
    endpoint: &str,
    instance: &str,
    pid: u32,
) -> Option<IAudioSessionControl2> {
    if pid == 0 {
        return None;
    }
    let mut current = current_sessions(manager, endpoint)?;
    let index = current
        .iter()
        .position(|(id, owner, _)| id == instance && *owner == pid)?;
    let (_, _, control) = current.swap_remove(index);
    (single_process(&control)? == pid).then_some(control)
}

unsafe fn observe_session(
    manager: &IMMDeviceEnumerator,
    endpoint: &str,
    instance: &str,
    pid: u32,
) -> Option<Value> {
    let control = verified_control(manager, endpoint, instance, pid)?;
    session_state(&control)?;
    // QUERY_LIMITED_INFORMATION | SYNCHRONIZE; no elevation or process writes.
    let raw = OpenProcess(0x1000 | 0x0010_0000, 0, pid);
    if raw.is_null() {
        return None;
    }
    let process = Handle(raw);
    let started = process_started(&process, pid)?;
    let image = LocalImage::from_process(&process)?;
    let mut icon = image.icon();
    let control = verified_control(manager, endpoint, instance, pid)?;
    let state = session_state(&control)?;
    let volume: ISimpleAudioVolume = control.cast().ok()?;
    let muted = volume.GetMute().ok()?.as_bool();
    if process_started(&process, pid)? != started
        || process_name(&process, 0)? != image.path
        || process_name(&process, 1)? != image.native
        || final_path(&image.process_file, 2)? != image.native
        || single_process(&control)? != pid
    {
        return None;
    }
    if !image.unchanged() {
        icon = None;
    }
    let basename = image.path.rsplit('\\').next()?;
    if !bounded_text(basename, 256) {
        return None;
    }
    Some(json!({
        "endpoint_id":endpoint,"session_instance_id":instance,"process_id":pid,
        "process_started":started.to_string(),"executable_path":image.path,
        "display_name":basename,"active":state == "active","session_state":state,
        "muted":muted,"icon_b64":icon.map(|bytes| base64(&bytes))
    }))
}

unsafe fn session_state(control: &IAudioSessionControl2) -> Option<&'static str> {
    match control.GetState().ok()? {
        AudioSessionStateActive => Some("active"),
        AudioSessionStateInactive => Some("inactive"),
        _ => None, // Expired/unknown is not a current selectable application.
    }
}

unsafe fn process_started(process: &Handle, pid: u32) -> Option<u64> {
    // A signalled process is dead, even if its exit code happens to be 259.
    if GetProcessId(process.0) != pid || WaitForSingleObject(process.0, 0) != 258 {
        return None;
    }
    let mut created = [0; 2];
    let mut exited = [0; 2];
    let mut kernel = [0; 2];
    let mut user = [0; 2];
    if GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) == 0 {
        return None;
    }
    let stamp = (u64::from(created[1]) << 32) | u64::from(created[0]);
    (stamp != 0 && WaitForSingleObject(process.0, 0) == 258).then_some(stamp)
}

unsafe fn process_name(process: &Handle, flags: u32) -> Option<String> {
    let mut buffer = [0u16; MAX_PATH];
    let mut length = buffer.len() as u32;
    if QueryFullProcessImageNameW(process.0, flags, buffer.as_mut_ptr(), &mut length) == 0
        || length == 0
        || length as usize >= buffer.len()
    {
        return None;
    }
    String::from_utf16(&buffer[..length as usize]).ok()
}

/// Deliberately conservative: no UNC, DOS devices, ADS, unresolved components,
/// control characters, trailing-dot aliases, or executable names without .exe.
fn local_executable_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bounded_text(path, MAX_PATH)
        && bytes.len() > 7
        && bytes[0].is_ascii_alphabetic()
        && bytes[1..3] == *b":\\"
        && !path[3..].contains(['/', ':', '*', '?', '"', '<', '>', '|'])
        && path[3..].split('\\').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.ends_with(['.', ' '])
        })
        && path
            .rsplit('.')
            .next()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
}

fn local_volume(volume: &str) -> bool {
    volume
        .strip_prefix("\\Device\\HarddiskVolume")
        .is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

unsafe fn drive_volume(path: &str) -> Option<String> {
    if !local_executable_path(path) {
        return None;
    }
    if GetDriveTypeW(wide(&path[..3]).as_ptr()) != 3 {
        return None; // DRIVE_FIXED only, never a mapped network/removable drive.
    }
    let mut buffer = [0u16; 512];
    let length = QueryDosDeviceW(wide(&path[..2]).as_ptr(), buffer.as_mut_ptr(), 512);
    if length == 0 || length as usize > buffer.len() {
        return None;
    }
    let end = buffer[..length as usize]
        .iter()
        .position(|unit| *unit == 0)?;
    let volume = String::from_utf16(&buffer[..end]).ok()?;
    local_volume(&volume).then_some(volume)
}

unsafe fn final_path(handle: &Handle, flags: u32) -> Option<String> {
    let mut buffer = [0u16; MAX_PATH + 64];
    let length =
        GetFinalPathNameByHandleW(handle.0, buffer.as_mut_ptr(), buffer.len() as u32, flags);
    if length == 0 || length as usize >= buffer.len() {
        return None;
    }
    String::from_utf16(&buffer[..length as usize]).ok()
}

/// The native target has already been proven to be a local HarddiskVolume.
/// OBJ_DONT_REPARSE fails before following *any* link, including a network link.
unsafe fn open_no_reparse(native: &str, directory: bool) -> Option<Handle> {
    let mut name: Vec<u16> = native.encode_utf16().collect();
    let length = u16::try_from(name.len().checked_mul(2)?).ok()?;
    let mut unicode = UnicodeString {
        length,
        maximum_length: length,
        buffer: name.as_mut_ptr(),
    };
    let mut attributes = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root_directory: ptr::null_mut(),
        object_name: &mut unicode,
        attributes: 0x40 | 0x1000, // OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE
        security_descriptor: ptr::null_mut(),
        security_quality_of_service: ptr::null_mut(),
    };
    let mut status = IoStatusBlock {
        status_or_pointer: 0,
        information: 0,
    };
    let mut handle = ptr::null_mut();
    // FILE_READ_ATTRIBUTES | SYNCHRONIZE, plus READ_DATA for the image.
    let access = 0x80 | 0x0010_0000 | if directory { 0 } else { 1 };
    // Synchronous, open reparse point itself, directory/non-directory as needed.
    let options = 0x20 | 0x0020_0000 | if directory { 1 } else { 0x40 };
    if NtOpenFile(
        &mut handle,
        access,
        &mut attributes,
        &mut status,
        1,
        options,
    ) != 0
        || handle.is_null()
    {
        return None;
    }
    let owned = Handle(handle);
    let stamp = file_stamp(&owned)?;
    if stamp[0] & 0x400 != 0 || (stamp[0] & 0x10 != 0) != directory {
        return None;
    }
    Some(owned)
}

unsafe fn file_stamp(handle: &Handle) -> Option<[u32; 13]> {
    let mut info = [0u32; 13];
    if GetFileInformationByHandle(handle.0, &mut info) == 0 {
        return None;
    }
    // Ignore last-access time; retain file ID, volume, creation, size and write.
    info[3] = 0;
    info[4] = 0;
    Some(info)
}

struct LocalImage {
    process_file: Handle,
    path: String,
    native: String,
    stamp: [u32; 13],
}

impl LocalImage {
    unsafe fn from_process(process: &Handle) -> Option<Self> {
        let path = process_name(process, 0)?;
        let native = process_name(process, 1)?;
        let volume = drive_volume(&path)?;
        if native != format!("{volume}{}", &path[2..]) {
            return None;
        }
        let process_file = open_no_reparse(&native, false)?;
        let canonical = final_path(&process_file, 0)?;
        let canonical = canonical.strip_prefix("\\\\?\\")?;
        if !local_executable_path(canonical)
            || !canonical.eq_ignore_ascii_case(&path)
            || final_path(&process_file, 2)? != native
            || drive_volume(canonical)? != volume
        {
            return None;
        }
        let stamp = file_stamp(&process_file)?;
        Some(Self {
            process_file,
            path,
            native,
            stamp,
        })
    }

    unsafe fn unchanged(&self) -> bool {
        file_stamp(&self.process_file) == Some(self.stamp)
            && final_path(&self.process_file, 2).as_deref() == Some(self.native.as_str())
    }

    unsafe fn icon(&self) -> Option<Vec<u8>> {
        let size =
            usize::try_from((u64::from(self.stamp[8]) << 32) | u64::from(self.stamp[9])).ok()?;
        if !(64..=MAX_IMAGE).contains(&size) || !self.unchanged() {
            return None;
        }
        // Map the already-held verified read-only file handle. Never reopen a
        // pathname or use a loader: ancestor changes cannot redirect this map,
        // and image imports, DllMain and shell handlers are never invoked.
        let raw = CreateFileMappingW(self.process_file.0, ptr::null(), 0x02, 0, 0, ptr::null()); // PAGE_READONLY
        if raw.is_null() {
            return None;
        }
        let mapping = Handle(raw);
        let address = MapViewOfFile(mapping.0, 0x0004, 0, 0, size); // FILE_MAP_READ
        if address.is_null() {
            return None;
        }
        let _view = MappedView(address);
        if !self.unchanged() {
            return None;
        }
        // File handle denies write/delete sharing for the mapping lifetime.
        let bytes = std::slice::from_raw_parts(address.cast::<u8>(), size);
        let icon = PeResources::new(bytes)?.icon()?;
        self.unchanged().then_some(icon)
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

struct PeResources<'a> {
    file: &'a [u8],
    sections: &'a [u8],
    root: &'a [u8],
}

impl<'a> PeResources<'a> {
    fn new(file: &'a [u8]) -> Option<Self> {
        if file.len() > MAX_IMAGE || file.get(..2)? != b"MZ" {
            return None;
        }
        let pe = u32_at(file, 0x3c)? as usize;
        if file.get(pe..pe.checked_add(4)?)? != b"PE\0\0" {
            return None;
        }
        let count = u16_at(file, pe.checked_add(6)?)? as usize;
        let optional_length = u16_at(file, pe.checked_add(20)?)? as usize;
        if !(1..=96).contains(&count) || optional_length > 4096 {
            return None;
        }
        let optional_start = pe.checked_add(24)?;
        let optional_end = optional_start.checked_add(optional_length)?;
        let optional = file.get(optional_start..optional_end)?;
        let directory = match u16_at(optional, 0)? {
            0x10b => 96usize,
            0x20b => 112usize,
            _ => return None,
        };
        if u32_at(optional, directory - 4)? < 3 {
            return None;
        }
        let resource_rva = u32_at(optional, directory + 16)?;
        let resource_size = u32_at(optional, directory + 20)? as usize;
        if !(16..=MAX_RESOURCE).contains(&resource_size) {
            return None;
        }
        let sections = file.get(optional_end..optional_end.checked_add(count.checked_mul(40)?)?)?;
        let mut parsed = Self {
            file,
            sections,
            root: &[],
        };
        parsed.root = parsed.rva_bytes(resource_rva, resource_size)?;
        Some(parsed)
    }

    fn rva_bytes(&self, rva: u32, length: usize) -> Option<&'a [u8]> {
        let mut found = None;
        for section in self.sections.chunks_exact(40) {
            let virtual_address = u32_at(section, 12)?;
            let raw_length = u32_at(section, 16)? as usize;
            let raw_address = u32_at(section, 20)? as usize;
            let Some(relative) = rva.checked_sub(virtual_address).map(|value| value as usize)
            else {
                continue;
            };
            if relative.checked_add(length)? > raw_length {
                continue;
            }
            let start = raw_address.checked_add(relative)?;
            let data = self.file.get(start..start.checked_add(length)?)?;
            if found.replace(data).is_some() {
                return None; // Overlapping/ambiguous PE sections are not trusted.
            }
        }
        found
    }

    fn directory(&self, offset: usize) -> Option<&'a [u8]> {
        let names = u16_at(self.root, offset.checked_add(12)?)? as usize;
        let ids = u16_at(self.root, offset.checked_add(14)?)? as usize;
        let count = names.checked_add(ids)?;
        if count > 128 {
            return None;
        }
        let start = offset.checked_add(16)?;
        let entries = self
            .root
            .get(start..start.checked_add(count.checked_mul(8)?)?)?;
        let mut numeric = BTreeSet::new();
        for (index, entry) in entries.chunks_exact(8).enumerate() {
            let name = u32_at(entry, 0)?;
            if index < names {
                if name & 0x8000_0000 == 0 {
                    return None;
                }
                let at = (name & 0x7fff_ffff) as usize;
                let characters = u16_at(self.root, at)? as usize;
                if characters > 260 {
                    return None;
                }
                let start = at.checked_add(2)?;
                self.root
                    .get(start..start.checked_add(characters.checked_mul(2)?)?)?;
            } else if name > u16::MAX as u32 || !numeric.insert(name) {
                return None;
            }
        }
        Some(entries)
    }

    fn id_entry(&self, directory: usize, id: u16) -> Option<u32> {
        self.directory(directory)?
            .chunks_exact(8)
            .find_map(|entry| {
                (u32_at(entry, 0)? == u32::from(id))
                    .then(|| u32_at(entry, 4))
                    .flatten()
            })
    }

    fn subdirectory(value: u32) -> Option<usize> {
        (value & 0x8000_0000 != 0).then_some((value & 0x7fff_ffff) as usize)
    }

    fn resource(&self, entry: u32, limit: usize) -> Option<&'a [u8]> {
        if entry & 0x8000_0000 != 0 {
            return None;
        }
        let entry = entry as usize;
        let rva = u32_at(self.root, entry)?;
        let length = u32_at(self.root, entry.checked_add(4)?)? as usize;
        if length == 0 || length > limit || u32_at(self.root, entry.checked_add(12)?)? != 0 {
            return None;
        }
        self.rva_bytes(rva, length)
    }

    fn icon(&self) -> Option<Vec<u8>> {
        let groups = Self::subdirectory(self.id_entry(0, 14)?)?; // RT_GROUP_ICON
        let icons = Self::subdirectory(self.id_entry(0, 3)?)?; // RT_ICON
        for group in self.directory(groups)?.chunks_exact(8).take(MAX_GROUPS) {
            let languages = Self::subdirectory(u32_at(group, 4)?)?;
            for language in self.directory(languages)?.chunks_exact(8).take(4) {
                let language_id = u16::try_from(u32_at(language, 0)?).ok()?;
                let group = self.resource(u32_at(language, 4)?, 6 + 14 * MAX_ICON_ENTRIES)?;
                if let Some(icon) = single_ico(group, |id| {
                    let languages = Self::subdirectory(self.id_entry(icons, id)?)?;
                    // Exact language in this mapped file only; no .mui fallback.
                    self.resource(self.id_entry(languages, language_id)?, MAX_ICON - 22)
                }) {
                    return Some(icon);
                }
            }
        }
        None
    }
}

fn single_ico<'a>(group: &[u8], read: impl Fn(u16) -> Option<&'a [u8]>) -> Option<Vec<u8>> {
    let count = u16_at(group, 4)? as usize;
    if u16_at(group, 0)? != 0
        || u16_at(group, 2)? != 1
        || !(1..=MAX_ICON_ENTRIES).contains(&count)
        || group.len() != 6 + 14 * count
    {
        return None;
    }
    let mut best: Option<(&[u8], &[u8], usize)> = None;
    for entry in group[6..].chunks_exact(14) {
        let width = usize::from(entry[0]);
        let height = usize::from(entry[1]);
        let length = u32_at(entry, 8)? as usize;
        let id = u16_at(entry, 12)?;
        if ![16, 24, 32].contains(&width)
            || height != width
            || entry[2] != 0
            || entry[3] != 0
            || u16_at(entry, 4)? > 1
            || ![0, 24, 32].contains(&u16_at(entry, 6)?)
            || id == 0
            || length == 0
            || length > MAX_ICON - 22
        {
            continue;
        }
        let Some(payload) = read(id) else { continue };
        if payload.len() != length || !valid_icon_payload(payload, width, height) {
            continue;
        }
        let area = width * height;
        if best.as_ref().is_none_or(|(_, _, old)| area > *old) {
            best = Some((entry, payload, area));
        }
    }
    let (entry, payload, _) = best?;
    let mut ico = Vec::with_capacity(22 + payload.len());
    ico.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    ico.extend_from_slice(&entry[..12]);
    ico.extend_from_slice(&22u32.to_le_bytes());
    ico.extend_from_slice(payload);
    Some(ico)
}

fn valid_icon_payload(bytes: &[u8], width: usize, height: usize) -> bool {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return valid_png(bytes, width, height);
    }
    // A conservative uncompressed 32-bit BITMAPINFOHEADER plus XOR/AND planes.
    // Other DIB encodings safely fall back to the placeholder.
    let xor = width * height * 4;
    let mask = width.div_ceil(32) * 4 * height;
    u32_at(bytes, 0) == Some(40)
        && u32_at(bytes, 4) == Some(width as u32)
        && u32_at(bytes, 8) == Some((height * 2) as u32)
        && u16_at(bytes, 12) == Some(1)
        && u16_at(bytes, 14) == Some(32)
        && u32_at(bytes, 16) == Some(0)
        && u32_at(bytes, 20).is_some_and(|size| [0, xor, xor + mask].contains(&(size as usize)))
        && u32_at(bytes, 32) == Some(0)
        && bytes.len() == 40 + xor + mask
}

fn valid_png(bytes: &[u8], width: usize, height: usize) -> bool {
    fn parse(bytes: &[u8], width: usize, height: usize) -> Option<()> {
        let mut offset = 8usize;
        let mut header = false;
        let mut image = false;
        let mut image_ended = false;
        while offset < bytes.len() {
            let length =
                u32::from_be_bytes(bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?)
                    as usize;
            let kind_start = offset.checked_add(4)?;
            let data_start = offset.checked_add(8)?;
            let data_end = data_start.checked_add(length)?;
            let end = data_end.checked_add(4)?;
            let kind = bytes.get(kind_start..data_start)?;
            let data = bytes.get(data_start..data_end)?;
            let expected = u32::from_be_bytes(bytes.get(data_end..end)?.try_into().ok()?);
            if !kind.iter().all(u8::is_ascii_alphabetic)
                || crc32(bytes.get(kind_start..data_end)?) != expected
                || (!header && kind != b"IHDR")
            {
                return None;
            }
            match kind {
                b"IHDR" => {
                    if header
                        || length != 13
                        || u32::from_be_bytes(data[..4].try_into().ok()?) != width as u32
                        || u32::from_be_bytes(data[4..8].try_into().ok()?) != height as u32
                        || data[8] != 8
                        || ![2, 6].contains(&data[9])
                        || data[10] != 0
                        || data[11] != 0
                        || data[12] > 1
                    {
                        return None;
                    }
                    header = true;
                }
                b"IDAT" => {
                    if image_ended {
                        return None;
                    }
                    image |= length != 0;
                }
                b"IEND" => return (length == 0 && image && end == bytes.len()).then_some(()),
                b"PLTE" => {
                    if image || length == 0 || length > 768 || !length.is_multiple_of(3) {
                        return None;
                    }
                }
                b"acTL" | b"fcTL" | b"fdAT" => return None, // Never animated.
                _ => {
                    if kind[0] & 0x20 == 0 {
                        return None;
                    }
                    image_ended |= image;
                }
            }
            offset = end;
        }
        None
    }
    parse(bytes, width, height).is_some()
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let bits = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(TABLE[((bits >> 18) & 63) as usize] as char);
        output.push(TABLE[((bits >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((bits >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(bits & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn set32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn dib(width: usize, height: usize) -> Vec<u8> {
        let mut bytes = vec![0; 40 + width * height * 4 + width.div_ceil(32) * 4 * height];
        set32(&mut bytes, 0, 40);
        set32(&mut bytes, 4, width as u32);
        set32(&mut bytes, 8, (height * 2) as u32);
        set16(&mut bytes, 12, 1);
        set16(&mut bytes, 14, 32);
        bytes
    }

    fn group(payload: &[u8], width: u8) -> Vec<u8> {
        let mut bytes = vec![0; 20];
        set16(&mut bytes, 2, 1);
        set16(&mut bytes, 4, 1);
        bytes[6] = width;
        bytes[7] = width;
        set16(&mut bytes, 10, 1);
        set16(&mut bytes, 12, 32);
        set32(&mut bytes, 14, payload.len() as u32);
        set16(&mut bytes, 18, 7);
        bytes
    }

    #[test]
    fn rejects_remote_namespace_ads_alias_and_unresolved_paths() {
        assert!(local_executable_path(r"C:\Program Files\Example\App.exe"));
        for path in [
            r"\\server\share\app.exe",
            r"\\?\C:\app.exe",
            r"\\.\C:\app.exe",
            r"C:app.exe",
            r"C:\..\app.exe",
            r"C:\.\app.exe",
            r"C:\app.exe:stream",
            r"C:\folder.\app.exe",
            r"C:\folder \app.exe",
            r"C:\\app.exe",
            r"C:/app.exe",
            r"C:\app.dll",
            "C:\\bad\n\\app.exe",
        ] {
            assert!(!local_executable_path(path), "{path:?}");
        }
        assert!(local_volume(r"\Device\HarddiskVolume12"));
        for volume in [
            r"\Device\Mup",
            r"\Device\HarddiskVolume1\redirect",
            r"\??\C:",
            r"\Device\HarddiskVolume",
            r"\Device\HarddiskVolumeX",
        ] {
            assert!(!local_volume(volume));
        }
    }

    #[test]
    fn creates_one_bounded_icon_with_exact_offset() {
        let payload = dib(32, 32);
        let group = group(&payload, 32);
        let ico = single_ico(&group, |id| (id == 7).then_some(payload.as_slice())).unwrap();
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 1, 0]);
        assert_eq!(u32_at(&ico, 18), Some(22));
        assert_eq!(&ico[22..], payload.as_slice());
        assert!(ico.len() <= MAX_ICON);
    }

    #[test]
    fn rejects_oversize_truncated_mismatched_and_bad_dib_icons() {
        let payload = dib(32, 32);
        let mut resource = group(&payload, 32);
        assert!(single_ico(&resource[..19], |_| Some(&payload)).is_none());
        resource[6] = 0; // ICO zero means 256, never 32.
        assert!(single_ico(&resource, |_| Some(&payload)).is_none());
        resource = group(&payload, 32);
        set32(&mut resource, 14, u32::MAX);
        assert!(single_ico(&resource, |_| Some(&payload)).is_none());
        resource = group(&payload, 16);
        assert!(single_ico(&resource, |_| Some(&payload)).is_none());
        let mut malformed = payload.clone();
        set32(&mut malformed, 16, 1); // Compressed DIB is not accepted.
        assert!(!valid_icon_payload(&malformed, 32, 32));
        assert!(!valid_icon_payload(&payload[..payload.len() - 1], 32, 32));
    }

    #[test]
    fn base64_padding_and_crc_are_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    fn png_chunk(bytes: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = bytes.len();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(data);
        let crc = crc32(&bytes[start..]);
        bytes.extend_from_slice(&crc.to_be_bytes());
    }

    #[test]
    fn png_checks_dimensions_chunks_crc_and_static_frame() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = [0; 13];
        ihdr[..4].copy_from_slice(&16u32.to_be_bytes());
        ihdr[4..8].copy_from_slice(&16u32.to_be_bytes());
        ihdr[8] = 8;
        ihdr[9] = 6;
        png_chunk(&mut png, b"IHDR", &ihdr);
        png_chunk(&mut png, b"IDAT", &[1]); // Header/structure test, not a PNG decoder.
        png_chunk(&mut png, b"IEND", &[]);
        assert!(valid_icon_payload(&png, 16, 16));
        assert!(!valid_icon_payload(&png, 32, 32));
        let mut corrupt = png.clone();
        corrupt[20] ^= 1;
        assert!(!valid_icon_payload(&corrupt, 16, 16));
        let mut animated = png[..33].to_vec();
        png_chunk(&mut animated, b"acTL", &[0; 8]);
        animated.extend_from_slice(&png[33..]);
        assert!(!valid_icon_payload(&animated, 16, 16));
        assert!(!valid_icon_payload(&png[..png.len() - 1], 16, 16));
    }

    fn pe_fixture() -> Vec<u8> {
        let payload = dib(16, 16);
        let group = group(&payload, 16);
        let mut file = vec![0u8; 0x200 + 256 + payload.len()];
        file[..2].copy_from_slice(b"MZ");
        set32(&mut file, 0x3c, 0x80);
        file[0x80..0x84].copy_from_slice(b"PE\0\0");
        set16(&mut file, 0x86, 1);
        set16(&mut file, 0x94, 224);
        set16(&mut file, 0x98, 0x10b);
        set32(&mut file, 0x98 + 92, 16);
        set32(&mut file, 0x98 + 112, 0x1000);
        let resource_length = (file.len() - 0x200) as u32;
        set32(&mut file, 0x98 + 116, resource_length);
        let section = 0x98 + 224;
        set32(&mut file, section + 8, resource_length);
        set32(&mut file, section + 12, 0x1000);
        set32(&mut file, section + 16, resource_length);
        set32(&mut file, section + 20, 0x200);
        let root = &mut file[0x200..];
        set16(root, 14, 2);
        set32(root, 16, 3);
        set32(root, 20, 0x8000_0020);
        set32(root, 24, 14);
        set32(root, 28, 0x8000_0050);
        set16(root, 32 + 14, 1);
        set32(root, 48, 7);
        set32(root, 52, 0x8000_0038);
        set16(root, 56 + 14, 1);
        set32(root, 72, 1033);
        set32(root, 76, 128);
        set16(root, 80 + 14, 1);
        set32(root, 96, 1);
        set32(root, 100, 0x8000_0068);
        set16(root, 104 + 14, 1);
        set32(root, 120, 1033);
        set32(root, 124, 144);
        set32(root, 128, 0x1100);
        set32(root, 132, payload.len() as u32);
        set32(root, 144, 0x10a0);
        set32(root, 148, group.len() as u32);
        root[160..180].copy_from_slice(&group);
        root[256..].copy_from_slice(&payload);
        file
    }

    #[test]
    fn parses_only_in_file_resources_and_rejects_malformed_offsets() {
        let fixture = pe_fixture();
        let icon = PeResources::new(&fixture).unwrap().icon().unwrap();
        assert_eq!(icon.len(), 22 + dib(16, 16).len());
        for offset in [
            0x3c,
            0x98 + 112,
            0x98 + 116,
            0x200 + 20,
            0x200 + 128,
            0x200 + 132,
            0x200 + 144,
        ] {
            let mut malformed = fixture.clone();
            set32(&mut malformed, offset, u32::MAX);
            assert!(PeResources::new(&malformed)
                .and_then(|pe| pe.icon())
                .is_none());
        }
        for length in [0, 1, 63, 0x80, 0x200, fixture.len() - 1] {
            assert!(PeResources::new(&fixture[..length])
                .and_then(|pe| pe.icon())
                .is_none());
        }
    }

    #[test]
    fn full_budget_fits_output_cap_and_errors_are_not_empty_success() {
        let session = json!({
            "endpoint_id":"e".repeat(MAX_ID),"session_instance_id":"s".repeat(MAX_ID),
            "executable_path":"p".repeat(MAX_PATH),"display_name":"d".repeat(256),
            "icon_b64":base64(&vec![0; MAX_ICON]),"process_started":u64::MAX.to_string(),
            "process_id":u32::MAX,"active":false,"muted":false,"session_state":"inactive"
        });
        let result = json!({"sessions":vec![session;MAX_SESSIONS],"endpoints":
            vec![json!({"id":"e".repeat(MAX_ID),"complete":false});MAX_ENDPOINTS]});
        assert!(serde_json::to_vec(&result).unwrap().len() < MAX_OUTPUT);
        let failure = unavailable("test");
        assert_eq!(failure["available"], false);
        assert_eq!(failure["complete"], false);
        assert!(failure["error"].is_string());
    }
}
