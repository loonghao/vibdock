//! Current-user startup configuration, independent of audio and saved bindings.
//! A Run value proves configuration only; Windows may delay or disable execution.
use serde_json::{json, Value};

pub(crate) fn parse_enabled(body: &str) -> Result<bool, &'static str> {
    if body.len() > 512 {
        return Err("Startup request is too large");
    }
    let value = crate::bindings::strict_json(body)
        .map_err(|_| "Expected an unambiguous startup JSON object")?;
    let object = value
        .as_object()
        .filter(|fields| fields.len() == 1 && fields.contains_key("enabled"))
        .ok_or("Only enabled is accepted")?;
    object["enabled"]
        .as_bool()
        .ok_or("enabled must be a boolean")
}

fn status(
    supported: bool,
    launcher_available: bool,
    state: &str,
    enabled: Option<bool>,
    confirmed: bool,
    error: Option<&str>,
) -> Value {
    json!({"platform":if supported{"windows"}else{"unsupported"},"supported":supported,"scope":if supported{Some("current_user")}else{None},"enabled":enabled,"state":state,"readback_confirmed":confirmed,"effective_state":"unknown","launcher_available":launcher_available,"error":error})
}

pub(crate) fn request(enabled: Option<bool>) -> (u16, Value) {
    #[cfg(windows)]
    {
        let settings = Settings::current();
        operate(&mut native::Backend, &settings, enabled)
    }
    #[cfg(not(windows))]
    {
        (
            if enabled.is_some() { 501 } else { 200 },
            status(
                false,
                false,
                "unsupported",
                None,
                false,
                Some("Current-user startup configuration requires Windows"),
            ),
        )
    }
}

#[cfg(any(windows, test))]
struct Settings {
    command: Option<String>,
    launcher_available: bool,
}
#[cfg(any(windows, test))]
impl Settings {
    fn from_executable(executable: &std::path::Path, launcher_exists: bool) -> Self {
        let command = executable.parent().and_then(|directory| {
            let launcher = directory.join("vibdock-launcher.exe");
            let path = launcher.to_str()?;
            if !launcher.is_absolute() || path.chars().any(|c| c.is_control() || c == '"') {
                return None;
            }
            let command = format!("\"{path}\" --startup");
            // Microsoft documents a 260-character command limit for Run values.
            (command.encode_utf16().count() <= 260).then_some(command)
        });
        Self {
            launcher_available: launcher_exists && command.is_some(),
            command,
        }
    }
    #[cfg(windows)]
    fn current() -> Self {
        match std::env::current_exe() {
            Ok(executable) => {
                let exists = executable
                    .parent()
                    .and_then(|directory| {
                        std::fs::symlink_metadata(directory.join("vibdock-launcher.exe")).ok()
                    })
                    .is_some_and(|metadata| {
                        metadata.is_file() && !metadata.file_type().is_symlink()
                    });
                Self::from_executable(&executable, exists)
            }
            Err(_) => Self {
                command: None,
                launcher_available: false,
            },
        }
    }
}
#[cfg(any(windows, test))]
trait Backend {
    fn read(&mut self) -> Result<Option<String>, String>;
    fn write(&mut self, command: Option<&str>) -> Result<(), String>;
}
#[cfg(any(windows, test))]
fn operate(
    backend: &mut impl Backend,
    settings: &Settings,
    requested: Option<bool>,
) -> (u16, Value) {
    let unknown = |error: &str| {
        (
            500,
            status(
                true,
                settings.launcher_available,
                "unknown",
                None,
                false,
                Some(error),
            ),
        )
    };
    let existing = match backend.read() {
        Ok(existing) => existing,
        Err(error) => {
            let (_, value) = unknown(&error);
            return (if requested.is_some() { 500 } else { 200 }, value);
        }
    };
    let current=match existing.as_deref() {
        None=>false,
        Some(value) if settings.command.as_deref()==Some(value)=>true,
        Some(_)=>return (if requested.is_some(){409}else{200},status(true,settings.launcher_available,"conflict",None,false,Some("Existing VibDock startup entry does not match this installation; it was preserved"))),
    };
    if let Some(enabled) = requested {
        if enabled && !settings.launcher_available {
            return (
                503,
                status(
                    true,
                    false,
                    "unknown",
                    None,
                    false,
                    Some("The service's sibling startup launcher is unavailable"),
                ),
            );
        }
        if enabled != current {
            if let Err(error) = backend.write(if enabled {
                settings.command.as_deref()
            } else {
                None
            }) {
                return unknown(&format!(
                    "Startup update failed; outcome unconfirmed: {error}"
                ));
            }
        }
        let readback = match backend.read() {
            Ok(value) => value,
            Err(error) => {
                return unknown(&format!(
                    "Startup readback failed; outcome unconfirmed: {error}"
                ))
            }
        };
        let expected = if enabled {
            settings.command.as_deref()
        } else {
            None
        };
        if readback.as_deref() != expected {
            return unknown(
                "Startup readback did not match the requested state; outcome unconfirmed",
            );
        }
        return (
            200,
            status(
                true,
                settings.launcher_available,
                if enabled { "enabled" } else { "disabled" },
                Some(enabled),
                true,
                None,
            ),
        );
    }
    (
        200,
        status(
            true,
            settings.launcher_available,
            if current { "enabled" } else { "disabled" },
            Some(current),
            true,
            None,
        ),
    )
}

#[cfg(windows)]
mod native {
    use windows::{
        core::{w, PCWSTR},
        Win32::{
            Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR},
            System::Registry::{
                RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
                RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE,
                REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE,
            },
        },
    };
    const KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    const NAME: PCWSTR = w!("VibDock");
    const MAX_VALUE_BYTES: usize = 4096;
    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }
    fn missing(error: WIN32_ERROR) -> bool {
        matches!(error, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
    }
    fn error(stage: &str, value: WIN32_ERROR) -> String {
        format!("{stage} (Windows error {})", value.0)
    }
    pub(super) struct Backend;
    impl super::Backend for Backend {
        fn read(&mut self) -> Result<Option<String>, String> {
            unsafe {
                let mut raw = HKEY::default();
                let opened = RegOpenKeyExW(HKEY_CURRENT_USER, KEY, None, KEY_QUERY_VALUE, &mut raw);
                if missing(opened) {
                    return Ok(None);
                }
                if opened != ERROR_SUCCESS {
                    return Err(error("Startup registry read unavailable", opened));
                }
                let key = Key(raw);
                let mut kind = REG_VALUE_TYPE::default();
                let mut bytes = [0u8; MAX_VALUE_BYTES];
                let mut length = bytes.len() as u32;
                let read = RegQueryValueExW(
                    key.0,
                    NAME,
                    None,
                    Some(&mut kind),
                    Some(bytes.as_mut_ptr()),
                    Some(&mut length),
                );
                if missing(read) {
                    return Ok(None);
                }
                if read != ERROR_SUCCESS {
                    return Err(error("Startup registry value unreadable", read));
                }
                if kind != REG_SZ
                    || length < 2
                    || length as usize > bytes.len()
                    || !length.is_multiple_of(2)
                {
                    return Err(
                        "Startup registry value has an unsupported format; it was preserved".into(),
                    );
                }
                let units = bytes[..length as usize]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect::<Vec<_>>();
                if units.last() != Some(&0) || units[..units.len() - 1].contains(&0) {
                    return Err("Startup registry value is malformed; it was preserved".into());
                }
                String::from_utf16(&units[..units.len() - 1])
                    .map(Some)
                    .map_err(|_| {
                        "Startup registry value is not valid UTF-16; it was preserved".into()
                    })
            }
        }
        fn write(&mut self, command: Option<&str>) -> Result<(), String> {
            unsafe {
                let mut raw = HKEY::default();
                let opened = if command.is_some() {
                    RegCreateKeyExW(
                        HKEY_CURRENT_USER,
                        KEY,
                        None,
                        None,
                        REG_OPTION_NON_VOLATILE,
                        KEY_SET_VALUE | KEY_QUERY_VALUE,
                        None,
                        &mut raw,
                        None,
                    )
                } else {
                    RegOpenKeyExW(HKEY_CURRENT_USER, KEY, None, KEY_SET_VALUE, &mut raw)
                };
                if command.is_none() && missing(opened) {
                    return Ok(());
                }
                if opened != ERROR_SUCCESS {
                    return Err(error("Startup registry update unavailable", opened));
                }
                let key = Key(raw);
                let written = if let Some(command) = command {
                    let bytes = command
                        .encode_utf16()
                        .chain(Some(0))
                        .flat_map(u16::to_le_bytes)
                        .collect::<Vec<_>>();
                    RegSetValueExW(key.0, NAME, None, REG_SZ, Some(&bytes))
                } else {
                    RegDeleteValueW(key.0, NAME)
                };
                if written == ERROR_SUCCESS || command.is_none() && missing(written) {
                    Ok(())
                } else {
                    Err(error("Startup registry update failed", written))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Fake {
        value: Option<String>,
        writes: usize,
        reads: usize,
        fail_write: bool,
        fail_readback: bool,
        mismatch: bool,
    }
    impl Backend for Fake {
        fn read(&mut self) -> Result<Option<String>, String> {
            self.reads += 1;
            if self.fail_readback && self.reads > 1 {
                Err("injected readback failure".into())
            } else if self.mismatch && self.reads > 1 {
                Ok(None)
            } else {
                Ok(self.value.clone())
            }
        }
        fn write(&mut self, command: Option<&str>) -> Result<(), String> {
            self.writes += 1;
            if self.fail_write {
                Err("injected write failure".into())
            } else {
                self.value = command.map(str::to_owned);
                Ok(())
            }
        }
    }
    fn settings() -> Settings {
        Settings {
            command: Some("\"C:\\Vib Dock\\vibdock-launcher.exe\" --startup".into()),
            launcher_available: true,
        }
    }
    #[test]
    fn startup_body_accepts_only_an_explicit_boolean() {
        assert_eq!(parse_enabled(r#"{"enabled":true}"#), Ok(true));
        assert_eq!(parse_enabled(r#"{"enabled":false}"#), Ok(false));
        for body in [
            "{}",
            "[]",
            "null",
            r#"{"enabled":null}"#,
            r#"{"enabled":1}"#,
            r#"{"enabled":"true"}"#,
            r#"{"enabled":true,"command":"arbitrary.exe"}"#,
            r#"{"enabled":false,"enabled":true}"#,
        ] {
            assert!(parse_enabled(body).is_err(), "{body}");
        }
    }
    #[test]
    fn startup_reads_never_enable_and_updates_require_matching_readback() {
        let settings = settings();
        let mut fake = Fake::default();
        let (code, value) = operate(&mut fake, &settings, None);
        assert_eq!(code, 200);
        assert_eq!(value["enabled"], false);
        assert_eq!(fake.writes, 0);
        let (code, value) = operate(&mut fake, &settings, Some(true));
        assert_eq!(code, 200);
        assert_eq!(value["enabled"], true);
        assert_eq!(value["readback_confirmed"], true);
        assert_eq!(value["effective_state"], "unknown");
        assert_eq!(fake.value, settings.command);
        let (code, value) = operate(&mut fake, &settings, Some(false));
        assert_eq!(code, 200);
        assert_eq!(value["enabled"], false);
        assert!(fake.value.is_none());
    }
    #[test]
    fn startup_conflicts_missing_launcher_and_failed_writes_do_not_claim_success() {
        let settings = settings();
        let mut fake = Fake {
            value: Some("unrecognized command".into()),
            ..Default::default()
        };
        for desired in [None, Some(true), Some(false)] {
            let (code, value) = operate(&mut fake, &settings, desired);
            assert_eq!(code, if desired.is_some() { 409 } else { 200 });
            assert_eq!(value["state"], "conflict");
            assert!(value["enabled"].is_null());
        }
        assert_eq!(fake.writes, 0);
        let absent = Settings {
            command: settings.command.clone(),
            launcher_available: false,
        };
        let mut fake = Fake::default();
        assert_eq!(operate(&mut fake, &absent, Some(true)).0, 503);
        assert_eq!(fake.writes, 0);
        assert_eq!(operate(&mut fake, &absent, None).1["enabled"], false);
        let mut fake = Fake {
            fail_write: true,
            ..Default::default()
        };
        let (code, value) = operate(&mut fake, &settings, Some(true));
        assert_eq!(code, 500);
        assert_eq!(value["readback_confirmed"], false);
        assert!(fake.value.is_none());
        for (fail_readback, mismatch) in [(true, false), (false, true)] {
            let mut fake = Fake {
                fail_readback,
                mismatch,
                ..Default::default()
            };
            let (code, value) = operate(&mut fake, &settings, Some(true));
            assert_eq!(code, 500);
            assert_eq!(value["readback_confirmed"], false);
            assert!(value["enabled"].is_null());
            assert_eq!(fake.writes, 1);
        }
    }
    #[test]
    fn startup_command_is_only_the_fixed_sibling_with_quoted_path() {
        let root = std::env::temp_dir().join("Vib Dock fixture");
        let executable = root.join("vibdock.exe");
        let settings = Settings::from_executable(&executable, true);
        assert_eq!(
            settings.command,
            Some(format!(
                "\"{}\" --startup",
                root.join("vibdock-launcher.exe").display()
            ))
        );
        assert!(
            Settings::from_executable(std::path::Path::new("relative/vibdock.exe"), true)
                .command
                .is_none()
        );
        assert!(Settings::from_executable(
            &std::env::temp_dir()
                .join("a".repeat(261))
                .join("vibdock.exe"),
            true
        )
        .command
        .is_none());
    }
    #[cfg(not(windows))]
    #[test]
    fn startup_other_platforms_report_unsupported_without_side_effects() {
        assert_eq!(request(None).1["state"], "unsupported");
        assert_eq!(request(Some(true)).0, 501);
        assert_eq!(request(Some(false)).0, 501);
    }
}
