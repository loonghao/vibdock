//! Version-gated internal Windows policy ABI, only in the disposable helper.
//! ABI reference: EarTrumpet 48e8958809ac736efdf6878656ab2c66ce323b7c.
//! Never call ClearAll; every requested role is an enum and is read back separately.
use super::{process_name, process_started, Handle, LocalImage, OpenProcess};
use crate::resident_apps::app_route::{self, Adapter, Job};
use serde_json::{json, Value};
use std::{ffi::c_void, ptr};
use windows::{
    core::{IInspectable_Vtbl, IUnknown, Interface, GUID, HRESULT, HSTRING, PCWSTR},
    Win32::{
        Media::Audio::{
            eCapture, eRender, IMMDeviceEnumerator, IMMEndpoint, MMDeviceEnumerator,
            DEVICE_STATE_ACTIVE,
        },
        System::{
            Com::{CoCreateInstance, CLSCTX_ALL},
            WinRT::{RoGetActivationFactory, RoInitialize, RoUninitialize, RO_INIT_SINGLETHREADED},
        },
    },
};

#[repr(C)]
struct PolicyVtbl {
    base: IInspectable_Vtbl,
    reserved: [usize; 19],
    set: unsafe extern "system" fn(*mut c_void, u32, i32, i32, *mut c_void) -> HRESULT,
    get: unsafe extern "system" fn(*mut c_void, u32, i32, i32, *mut *mut c_void) -> HRESULT,
    clear_all: usize,
}
#[repr(transparent)]
#[derive(Clone)]
struct Downlevel(IUnknown);
unsafe impl Interface for Downlevel {
    type Vtable = PolicyVtbl;
    const IID: GUID = GUID::from_u128(0x2a59116d_6c4f_45e0_a74f_707e3fef9258);
}
#[repr(transparent)]
#[derive(Clone)]
struct Modern(IUnknown);
unsafe impl Interface for Modern {
    type Vtable = PolicyVtbl;
    const IID: GUID = GUID::from_u128(0xab3d4648_e242_459f_b02f_541c70306324);
}
enum Policy {
    Downlevel(Downlevel),
    Modern(Modern),
}
impl Policy {
    unsafe fn open(build: u32) -> Result<Self, String> {
        if build < 17134 {
            return Err("preferred_routing_unsupported_windows_version".into());
        }
        let name = HSTRING::from("Windows.Media.Internal.AudioPolicyConfig");
        if build >= 21390 {
            RoGetActivationFactory::<Modern>(&name).map(Self::Modern)
        } else {
            RoGetActivationFactory::<Downlevel>(&name).map(Self::Downlevel)
        }
        .map_err(|e| code("preferred_routing_interface_unavailable", e))
    }
    fn interface(&self) -> (*mut c_void, &PolicyVtbl) {
        match self {
            Self::Downlevel(value) => (value.as_raw(), value.vtable()),
            Self::Modern(value) => (value.as_raw(), value.vtable()),
        }
    }
    unsafe fn get(&self, pid: u32, flow: i32, role: u32) -> Result<Option<String>, String> {
        let (this, vtable) = self.interface();
        let mut raw = ptr::null_mut();
        let status = (vtable.get)(this, pid, flow, role as i32, &mut raw);
        // HSTRING is an owned, pointer-sized ABI value; adopt even on failure so
        // a returned allocation is released. No arbitrary string pointer read.
        let value = std::mem::transmute::<*mut c_void, HSTRING>(raw);
        status
            .ok()
            .map_err(|e| code("preferred_routing_read_failed", e))?;
        if value.is_empty() {
            return Ok(None);
        }
        if value.len() > 1024 {
            return Err("preferred_routing_read_budget_exceeded".into());
        }
        String::from_utf16(&value)
            .map(Some)
            .map_err(|_| "preferred_routing_read_invalid".into())
    }
    unsafe fn set(
        &self,
        pid: u32,
        flow: i32,
        role: u32,
        value: Option<&str>,
    ) -> Result<(), String> {
        let string = value.map(HSTRING::from).unwrap_or_default();
        let raw = std::mem::transmute_copy::<HSTRING, *mut c_void>(&string);
        let (this, vtable) = self.interface();
        (vtable.set)(this, pid, flow, role as i32, raw)
            .ok()
            .map_err(|e| code("preferred_routing_write_failed", e))
    }
}
fn code(stage: &str, error: windows::core::Error) -> String {
    format!("{stage} (0x{:08X})", error.code().0 as u32)
}
#[repr(C)]
struct Version {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    service_pack: [u16; 128],
}
#[link(name = "ntdll")]
extern "system" {
    fn RtlGetVersion(version: *mut Version) -> i32;
}
fn build() -> Option<u32> {
    let mut version = Version {
        size: std::mem::size_of::<Version>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        service_pack: [0; 128],
    };
    (unsafe { RtlGetVersion(&mut version) } >= 0 && version.major >= 10).then_some(version.build)
}
struct Runtime;
impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}
unsafe fn initialize() -> Result<Runtime, String> {
    RoInitialize(RO_INIT_SINGLETHREADED)
        .map_err(|e| code("preferred_routing_runtime_unavailable", e))?;
    Ok(Runtime)
}
fn flow(value: &str) -> i32 {
    if value == "capture" {
        eCapture.0
    } else {
        eRender.0
    }
}

/// Read-only exact-QI and all-role probe. This is availability evidence for the
/// adapter, never evidence that any third-party stream obeys the preference.
pub(super) fn capability() -> Value {
    let mut value = json!({"state":"unknown","preferred_input":"unknown","preferred_output":"unknown","stream_state":"unknown","app_microphone_mute":"unsupported","error":"preferred_routing_probe_unavailable"});
    unsafe {
        let probe = (|| -> Result<(), String> {
            let _runtime = initialize()?;
            let build = build().ok_or("windows_version_unavailable")?;
            if build < 17134 {
                value["state"] = json!("unsupported");
                return Err("preferred_routing_unsupported_windows_version".into());
            }
            let policy = Policy::open(build)?;
            for direction in [eCapture.0, eRender.0] {
                for role in [0, 1, 2] {
                    policy.get(std::process::id(), direction, role)?;
                }
            }
            value["state"] = json!("available");
            value["preferred_input"] = json!("available");
            value["preferred_output"] = json!("available");
            value["error"] = Value::Null;
            Ok(())
        })();
        if let Err(error) = probe {
            value["error"] = json!(error);
        }
    }
    value
}

struct Native<'a> {
    job: &'a Job,
    process: Handle,
    image: LocalImage,
    policy: Policy,
    manager: IMMDeviceEnumerator,
}

/// Bounded endpoint names and identities for selectors; no volume/capture change.
pub(super) fn devices() -> Value {
    let mut value =
        json!({"capture":[],"render":[],"available":false,"complete":false,"error":null});
    unsafe {
        let scan = (|| -> Result<(), String> {
            let _runtime = initialize()?;
            let manager: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| code("route_endpoint_enumerator_unavailable", e))?;
            let mut complete = true;
            for (name, direction) in [("capture", eCapture), ("render", eRender)] {
                let devices = manager
                    .EnumAudioEndpoints(direction, DEVICE_STATE_ACTIVE)
                    .map_err(|e| code("route_endpoint_list_unavailable", e))?;
                let count = devices
                    .GetCount()
                    .map_err(|e| code("route_endpoint_count_unavailable", e))?;
                if count > 16 {
                    complete = false;
                }
                let mut rows = Vec::new();
                for index in 0..count.min(16) {
                    let row = (|| -> Option<Value> {
                        let device = devices.Item(index).ok()?;
                        let id = crate::windows::device_id(&device).ok()?;
                        let label = crate::windows::device_name(&device).ok()?;
                        if !super::bounded_text(&id, 512) || !super::bounded_text(&label, 160) {
                            return None;
                        }
                        let endpoint: IMMEndpoint = device.cast().ok()?;
                        if endpoint.GetDataFlow().ok()? != direction
                            || device.GetState().ok()? != DEVICE_STATE_ACTIVE
                            || crate::windows::device_id(&device).ok().as_deref()
                                != Some(id.as_str())
                        {
                            return None;
                        }
                        Some(json!({"id":id,"name":label}))
                    })();
                    if let Some(row) = row {
                        if rows.iter().any(|old: &Value| old["id"] == row["id"]) {
                            complete = false;
                        } else {
                            rows.push(row);
                        }
                    } else {
                        complete = false;
                    }
                }
                value[name] = json!(rows);
            }
            value["available"] = json!(true);
            value["complete"] = json!(complete);
            Ok(())
        })();
        if let Err(error) = scan {
            value["error"] = json!(error);
            value["capture"] = json!([]);
            value["render"] = json!([]);
        }
    }
    value
}
impl Native<'_> {
    unsafe fn destination(&self) -> Result<(), String> {
        let wide: Vec<u16> = self.job.device_id.encode_utf16().chain(Some(0)).collect();
        let device = self
            .manager
            .GetDevice(PCWSTR(wide.as_ptr()))
            .map_err(|e| code("route_endpoint_unavailable", e))?;
        if crate::windows::device_id(&device).ok().as_deref() != Some(self.job.device_id.as_str()) {
            return Err("route_endpoint_identity_changed".into());
        }
        let endpoint: IMMEndpoint = device
            .cast()
            .map_err(|e| code("route_endpoint_flow_unavailable", e))?;
        if endpoint
            .GetDataFlow()
            .map_err(|e| code("route_endpoint_flow_unavailable", e))?
            .0
            != flow(&self.job.flow)
        {
            return Err("route_endpoint_direction_mismatch".into());
        }
        if device
            .GetState()
            .map_err(|e| code("route_endpoint_state_unavailable", e))?
            != DEVICE_STATE_ACTIVE
        {
            return Err("route_endpoint_disconnected_or_disabled".into());
        }
        Ok(())
    }
}
impl Adapter for Native<'_> {
    fn guard(&mut self) -> Result<(), String> {
        unsafe {
            if process_started(&self.process, self.job.process_id) != Some(self.job.process_started)
                || process_name(&self.process, 0).as_deref()
                    != Some(self.job.executable_path.as_str())
                || process_name(&self.process, 1).as_deref() != Some(self.image.native.as_str())
                || !self.image.unchanged()
            {
                Err("route_process_identity_changed".into())
            } else {
                Ok(())
            }
        }
    }
    fn get(&mut self, role: u32) -> Result<Option<String>, String> {
        unsafe {
            self.policy
                .get(self.job.process_id, flow(&self.job.flow), role)
        }
    }
    fn set(&mut self, role: u32, value: Option<&str>) -> Result<(), String> {
        unsafe {
            // Restoring a snapshot may refer to an unplugged previous endpoint. Its
            // exact old policy is restored; never substitute any current default.
            if value == app_route::pack(&self.job.device_id, &self.job.flow).as_deref() {
                self.destination()?;
            }
            self.guard()?;
            self.policy
                .set(self.job.process_id, flow(&self.job.flow), role, value)
        }
    }
}
pub(super) fn execute(job: &Job) -> Value {
    unsafe {
        let operation = (|| -> Result<Value, String> {
            let _runtime = initialize()?;
            let policy = Policy::open(build().ok_or("windows_version_unavailable")?)?;
            let process = Handle(OpenProcess(0x0010_1000, 0, job.process_id));
            if process.0.is_null() {
                return Err("route_process_unavailable".into());
            }
            if process_started(&process, job.process_id) != Some(job.process_started) {
                return Err("route_process_identity_changed".into());
            }
            let image =
                LocalImage::from_process(&process).ok_or("route_process_image_unavailable")?;
            if image.path != job.executable_path {
                return Err("route_process_identity_changed".into());
            }
            let manager = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| code("route_endpoint_enumerator_unavailable", e))?;
            let mut native = Native {
                job,
                process,
                image,
                policy,
                manager,
            };
            native.guard()?;
            native.destination()?;
            Ok(app_route::apply(&mut native, job))
        })();
        operation.unwrap_or_else(|error| app_route::failure(job, false, &error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_slots_follow_inspectable_and_nineteen_reserved_methods() {
        let pointer = std::mem::size_of::<usize>();
        assert_eq!(std::mem::offset_of!(PolicyVtbl, set), (6 + 19) * pointer);
        assert_eq!(std::mem::offset_of!(PolicyVtbl, get), (6 + 20) * pointer);
        assert_eq!(std::mem::size_of::<Version>(), 276);
        assert_ne!(Downlevel::IID, Modern::IID);
        assert_eq!(flow("render"), 0);
        assert_eq!(flow("capture"), 1);
    }
}
