//! Windows launcher/tray entry point: a GUI-subsystem executable with no
//! console flash on double-click.
//!
//! This binary is deliberately thin. It ensures a resident service is running
//! (reusing an authenticated one if there is), opens that service's actual
//! bound URL, and then stays in the notification area as the way back to the
//! page and the way to stop the service. Closing the browser leaves the
//! service running; only the tray Exit item stops it.

// The launcher is GUI-subsystem: double-clicking must not flash a console.
#![cfg_attr(windows, windows_subsystem = "windows")]

// The tray and the notification area it lives in are Windows concepts. On
// other platforms the launcher has nothing to host, so it reports that rather
// than pretending to start.
#[cfg(windows)]
use vibdock::tray;
use vibdock::Result;

fn main() {
    if let Err(error) = run() {
        // No console to print to, so a startup failure must be visible.
        report_error(&error.to_string());
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    #[cfg(not(windows))]
    {
        // No notification area to host the tray, so the launcher cannot
        // fulfil its contract here. Saying so is honest; pretending to start
        // is not.
        Err("The VibDock launcher is a Windows tray entry and is not supported on this platform. Use `vibdock service-start` to run the resident service.".into())
    }
    #[cfg(windows)]
    {
        // Ensure a service exists before showing the tray, so Open always has a
        // real bound URL to navigate to and the icon always controls something.
        //
        // This launcher does NOT claim the port itself: the service it starts
        // is a separate process that must bind that same port, so a claim held
        // here would deadlock the child. The child claims during its own
        // startup; this process only waits for a service to prove itself.
        start_detached_service()?;
        wait_for_service()?;
        let url = vibdock::configuration_url()?;
        open_url(&url);
        tray::run()
    }
}

/// Starts the resident service as a separate, detached process.
///
/// The service must outlive this launcher so that closing the page — or the
/// tray itself — leaves it running. A spawned thread would die with this
/// process; a detached `vibdock service-start` does not.
///
/// The child is the sibling **console** executable, not this one: this launcher
/// is a GUI-subsystem binary that ignores argv, so re-executing it would spawn
/// another launcher instead of a service.
#[cfg(windows)]
fn start_detached_service() -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    // DETACHED_PROCESS (0x00000008) | CREATE_NEW_PROCESS_GROUP (0x00000200):
    // the child gets no console and is not signalled with this process.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    let cli = sibling_cli()?;
    let mut command = std::process::Command::new(cli);
    command
        .arg("service-start")
        // Null stdio: the child is a detached resident process, and a pipe
        // held open here would keep this launcher waiting on its output.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    command.spawn().map(|_| ()).map_err(|e| e.into())
}

/// Path of the sibling console executable that can host the service.
///
/// This launcher ignores its own arguments, so starting the service means
/// running the console binary next to it. A missing sibling is a visible
/// error: silently falling back would either recurse into another launcher or
/// leave the user with a tray that controls nothing.
#[cfg(windows)]
fn sibling_cli() -> Result<std::path::PathBuf> {
    let launcher = std::env::current_exe()?;
    let cli = launcher.with_file_name("vibdock.exe");
    if !cli.is_file() {
        return Err(format!(
            "Cannot start the resident service: the console executable vibdock.exe was not found \
             next to the launcher at {}.",
            launcher.display()
        )
        .into());
    }
    Ok(cli)
}

/// Waits until the service we just started is answering.
#[cfg(windows)]
fn wait_for_service() -> Result<()> {
    for _ in 0..100 {
        if vibdock::service::live_instance().is_some() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err("The resident service did not start.".into())
}

/// Opens a URL in the user's default browser.
///
/// Failure is not fatal: the tray remains, so the user can still open the page
/// from there or exit.
#[cfg(windows)]
fn open_url(url: &str) {
    #[cfg(windows)]
    {
        use windows::{
            core::PCWSTR,
            Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        };
        let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        unsafe {
            let _ = ShellExecuteW(
                None,
                PCWSTR(verb.as_ptr()),
                PCWSTR(wide.as_ptr()),
                None,
                None,
                SW_SHOWNORMAL,
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = url;
    }
}

/// Shows an error dialog, since a GUI-subsystem binary has no console.
fn report_error(message: &str) {
    #[cfg(windows)]
    {
        use windows::{
            core::PCWSTR,
            Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK},
        };
        let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
        let title: Vec<u16> = "VibDock\0".encode_utf16().collect();
        unsafe {
            let _ = MessageBoxW(
                None,
                PCWSTR(text.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("ERROR: {message}");
    }
}
