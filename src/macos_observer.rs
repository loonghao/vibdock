//! Bounded, read-only observations of current-user macOS applications.
//!
//! Run only on the main thread of the disposable observer helper. AppKit may
//! block while resolving a native icon; the parent owns the hard deadline and
//! output limit. No saved path is accepted here, and no application is opened,
//! activated, terminated, or inspected for audio/voice/agent activity.

use std::{
    ffi::c_void,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

use objc2::{
    rc::{autoreleasepool, Retained},
    AnyThread,
};
use objc2_app_kit::{
    NSBitmapFormat, NSBitmapImageRep, NSCompositingOperation, NSDeviceRGBColorSpace,
    NSGraphicsContext, NSImage, NSImageInterpolation, NSRunningApplication, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSData, NSDataBase64EncodingOptions, NSDate, NSDefaultRunLoopMode, NSPoint,
    NSRect, NSRunLoop, NSSize, NSString, NSURL,
};
use serde_json::{json, Value};

const MAX_INSPECTED: usize = 128;
const MAX_APPLICATIONS: usize = 64;
const MAX_PATH_BYTES: usize = 2048;
const MAX_DISPLAY_BYTES: usize = 160;
const MAX_ICON_BYTES: usize = 16 * 1024;
const ICON_SIDE: usize = 32;
const ICON_ROW: usize = ICON_SIDE * 4;
const ICON_RGBA_BYTES: usize = ICON_ROW * ICON_SIDE;
const COLLECTION_BUDGET: Duration = Duration::from_millis(1500);
const SOFT_BUDGET: Duration = Duration::from_secs(2);

// Published Darwin SDK ABI, not process enumeration or privileged task-port access.
// Headers: https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/proc_info.h
// https://github.com/apple-oss-distributions/xnu/blob/main/libsyscall/wrappers/libproc/libproc.h
// PROC_PIDTBSDINFO = 3; proc_bsdinfo is 136 bytes on macOS arm64/x86_64.
#[repr(C)]
#[derive(Default)]
struct BsdInfo {
    flags: u32,
    status: u32,
    exit_status: u32,
    pid: u32,
    ppid: u32,
    uid: u32,
    gid: u32,
    ruid: u32,
    rgid: u32,
    svuid: u32,
    svgid: u32,
    reserved: u32,
    comm: [u8; 16],
    name: [u8; 32],
    files: u32,
    pgid: u32,
    job_control: u32,
    tty_device: u32,
    tty_pgid: u32,
    nice: i32,
    started_sec: u64,
    started_usec: u64,
}
const _: [(); 136] = [(); std::mem::size_of::<BsdInfo>()];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessIdentity {
    uid: u32,
    started_sec: u64,
    started_usec: u64,
}

#[link(name = "proc")]
extern "C" {
    fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut c_void, size: i32) -> i32;
    fn proc_pidpath(pid: i32, buffer: *mut c_void, size: u32) -> i32;
}
extern "C" {
    fn getuid() -> u32;
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    pid: i32,
    process: ProcessIdentity,
    started: String,
    bundle_path: String,
    bundle_id: Option<String>,
    // Revalidate the native URL even when the process executable is unreadable.
    executable_url: Option<String>,
    executable_path: Option<String>,
}

struct Candidate {
    application: Retained<NSRunningApplication>,
    identity: Identity,
    display_name: String,
    icon_b64: Option<String>,
}

/// Process presence only. A complete result concerns applications in this
/// user's workspace, including accessory/background apps, never every process,
/// playback, or application work.
/// Refuse a background-thread call before touching AppKit.
pub(crate) fn snapshot() -> Value {
    let Some(_main_thread) = MainThreadMarker::new() else {
        return json!({
            "available": false, "complete": false, "skipped": 0,
            "error": "Application observation requires the helper main thread.",
            "applications": []
        });
    };
    autoreleasepool(|_| collect_snapshot())
}

fn collect_snapshot() -> Value {
    let started = Instant::now();
    // SAFETY: getuid has no arguments or side effects and returns Darwin uid_t.
    let uid = unsafe { getuid() };
    let running = NSWorkspace::sharedWorkspace().runningApplications();
    let count = running.count();
    let inspected = count.min(MAX_INSPECTED);
    let mut skipped = count.saturating_sub(inspected);
    let mut candidates = Vec::new();
    for index in 0..inspected {
        if started.elapsed() >= COLLECTION_BUDGET || candidates.len() == MAX_APPLICATIONS {
            skipped += inspected - index;
            break;
        }
        let application = running.objectAtIndex(index);
        // Dock/menu visibility is not process presence. A pinned application
        // may become accessory/background while continuing to run; never filter
        // it out based on activation policy or whether it is frontmost.
        // Explicitly exclude another user's process before resolving its paths
        // or icon. An unreadable identity is conservatively an incomplete read.
        match process_identity(application.processIdentifier()) {
            Some(process) if process.uid != uid => continue,
            Some(_) => {}
            None => {
                skipped += 1;
                continue;
            }
        }
        let Some(identity) = read_identity(&application, uid) else {
            skipped += 1;
            continue;
        };
        let display_name = application
            .localizedName()
            .and_then(|name| bounded_string(&name, MAX_DISPLAY_BYTES))
            .or_else(|| {
                Path::new(&identity.bundle_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(display_label)
            })
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Application".into());
        let icon_b64 = application.icon().and_then(|icon| rasterize_icon(&icon));
        candidates.push(Candidate {
            application,
            identity,
            display_name,
            icon_b64,
        });
    }

    // NSRunningApplication caches changing properties until a common-mode turn
    // of the main run loop. One non-waiting default-mode turn invalidates that
    // snapshot before re-reading the retained objects and fresh PID lookups.
    // SAFETY: This process owns its main thread; the framework constant is valid.
    unsafe {
        NSRunLoop::mainRunLoop().runMode_beforeDate(NSDefaultRunLoopMode, &NSDate::date());
    }
    let mut applications = Vec::new();
    for candidate in candidates {
        if started.elapsed() >= SOFT_BUDGET
            || !still_same_application(&candidate.application, &candidate.identity, uid)
        {
            skipped += 1;
            continue;
        }
        let identity = candidate.identity;
        applications.push(json!({
            "bundle_path": identity.bundle_path,
            "bundle_id": identity.bundle_id,
            "executable_path": identity.executable_path,
            "process_id": identity.pid,
            "process_started": identity.started,
            "display_name": candidate.display_name,
            "running": true,
            "icon_b64": candidate.icon_b64
        }));
    }
    let complete = skipped == 0;
    json!({
        "available": true, "complete": complete, "skipped": skipped,
        "error": if complete { None } else {
            Some("Application observation is partial; unobserved applications are unknown.")
        },
        "applications": applications
    })
}

fn read_identity(application: &NSRunningApplication, uid: u32) -> Option<Identity> {
    if application.isTerminated() {
        return None;
    }
    let pid = application.processIdentifier();
    let process = process_identity(pid)?;
    if process.uid != uid {
        return None;
    }
    let started = launch_stamp(application.launchDate()?.timeIntervalSince1970())?;
    let bundle_path = canonical_url(&application.bundleURL()?)?;
    if !is_bundle_path(&bundle_path) || !bundle_path.is_dir() {
        return None;
    }
    let bundle_id = application
        .bundleIdentifier()
        .map(|id| bounded_string(&id, 255))
        .transpose_option()?;
    let executable_url = application
        .executableURL()
        .map(|url| canonical_url(&url))
        .transpose_option()?;
    let executable_path = executable_url.as_ref().and_then(|path| {
        (is_bundle_executable(&bundle_path, path) && process_executable(pid).as_ref() == Some(path))
            .then(|| path.to_str().unwrap().to_owned())
    });
    let identity = Identity {
        pid,
        process,
        started,
        bundle_path: bundle_path.to_str()?.to_owned(),
        bundle_id,
        executable_url: executable_url.map(|p| p.to_str().unwrap().to_owned()),
        executable_path,
    };
    (!application.isTerminated() && process_identity(pid) == Some(process)).then_some(identity)
}

// Option<Option<T>>: an absent native optional value is valid; a present but
// malformed value fails the observation instead of being silently downgraded.
trait TransposeOption<T> {
    fn transpose_option(self) -> Option<Option<T>>;
}
impl<T> TransposeOption<T> for Option<Option<T>> {
    fn transpose_option(self) -> Option<Option<T>> {
        match self {
            None => Some(None),
            Some(value) => value.map(Some),
        }
    }
}

fn still_same_application(original: &NSRunningApplication, expected: &Identity, uid: u32) -> bool {
    let Some(current) = NSRunningApplication::runningApplicationWithProcessIdentifier(expected.pid)
    else {
        return false;
    };
    // NSRunningApplication equality is Apple's process-instance comparison;
    // PID alone is explicitly insufficient. Check both objects after the run
    // loop turn, including launchDate and canonical bundle/executable URLs.
    original == &*current
        && read_identity(original, uid).as_ref() == Some(expected)
        && read_identity(&current, uid).as_ref() == Some(expected)
        && !current.isTerminated()
}

fn launch_stamp(seconds: f64) -> Option<String> {
    (seconds.is_finite() && seconds > 0.0).then(|| format!("{:016x}", seconds.to_bits()))
}

fn bounded_string(value: &NSString, max_bytes: usize) -> Option<String> {
    // Bound UTF-16 length before making a Rust allocation, then bound UTF-8.
    if value.length() == 0 || value.length() > max_bytes {
        return None;
    }
    let text = value.to_string();
    (text.len() <= max_bytes && !text.chars().any(char::is_control)).then_some(text)
}

fn display_label(value: &str) -> String {
    let mut result = String::new();
    for ch in value.chars().filter(|ch| !ch.is_control()) {
        if result.len() + ch.len_utf8() > MAX_DISPLAY_BYTES {
            break;
        }
        result.push(ch);
    }
    result
}

fn canonical_url(url: &NSURL) -> Option<PathBuf> {
    if !url.isFileURL() {
        return None;
    }
    if let Some(host) = url.host() {
        if host.length() > 9 {
            return None;
        }
        let host = host.to_string();
        if !host.is_empty() && host != "localhost" {
            return None;
        }
    }
    let path = bounded_string(&url.path()?, MAX_PATH_BYTES)?;
    canonical_local_path(&path)
}

fn canonical_local_path(path: &str) -> Option<PathBuf> {
    if !is_absolute_local_path(path) {
        return None;
    }
    let canonical = std::fs::canonicalize(path).ok()?;
    is_absolute_local_path(canonical.to_str()?).then_some(canonical)
}

fn is_absolute_local_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && path.starts_with('/')
        && !path.starts_with("//")
        && !path.chars().any(char::is_control)
        && !Path::new(path)
            .components()
            .any(|p| matches!(p, Component::CurDir | Component::ParentDir))
        && !path.split('/').any(|part| part == "." || part == "..")
}

fn is_bundle_path(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "app")
        && path.file_stem().is_some_and(|name| !name.is_empty())
}

fn is_bundle_executable(bundle: &Path, executable: &Path) -> bool {
    let root = bundle.join("Contents/MacOS");
    executable.starts_with(&root)
        && executable != root
        && std::fs::metadata(executable)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn process_identity(pid: i32) -> Option<ProcessIdentity> {
    if pid <= 0 {
        return None;
    }
    let mut info = BsdInfo::default();
    // SAFETY: Matching published Darwin SDK ABI; the writable buffer has the exact
    // declared 136-byte size and is checked for a complete kernel response.
    let size = unsafe {
        proc_pidinfo(
            pid,
            3,
            0,
            (&mut info as *mut BsdInfo).cast(),
            std::mem::size_of::<BsdInfo>() as i32,
        )
    };
    (size == std::mem::size_of::<BsdInfo>() as i32
        && info.pid == pid as u32
        && info.status != 5 // SZOMB
        && info.flags & 5 == 0 // PROC_FLAG_SYSTEM | PROC_FLAG_INEXIT
        && info.uid == info.ruid
        && info.started_sec > 0
        && info.started_usec < 1_000_000)
        .then_some(ProcessIdentity {
            uid: info.uid,
            started_sec: info.started_sec,
            started_usec: info.started_usec,
        })
}

fn process_executable(pid: i32) -> Option<PathBuf> {
    if pid <= 0 {
        return None;
    }
    // PROC_PIDPATHINFO_MAXSIZE is 4 * MAXPATHLEN (4096) on Darwin.
    let mut bytes = [0_u8; 4096];
    // SAFETY: The writable buffer and exact capacity are valid for this call.
    let length = unsafe { proc_pidpath(pid, bytes.as_mut_ptr().cast(), bytes.len() as u32) };
    if length <= 0 || length as usize >= bytes.len() {
        return None;
    }
    let end = bytes.iter().position(|b| *b == 0)?;
    canonical_local_path(std::str::from_utf8(&bytes[..end]).ok()?)
}

struct GraphicsState;
impl Drop for GraphicsState {
    fn drop(&mut self) {
        NSGraphicsContext::restoreGraphicsState_class();
    }
}

fn rasterize_icon(icon: &NSImage) -> Option<String> {
    let source = icon.size();
    if !source.width.is_finite()
        || !source.height.is_finite()
        || source.width < 1.0
        || source.height < 1.0
        || source.width > 4096.0
        || source.height > 4096.0
    {
        return None;
    }
    // SAFETY: Null planes asks AppKit to allocate its own fixed-size bitmap.
    // No original icon data/representations or file paths are serialized.
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            ICON_SIDE as isize,
            ICON_SIDE as isize,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat::empty(),
            ICON_ROW as isize,
            32,
        )?
    };
    if bitmap.pixelsWide() != ICON_SIDE as isize
        || bitmap.pixelsHigh() != ICON_SIDE as isize
        || bitmap.bitsPerSample() != 8
        || bitmap.samplesPerPixel() != 4
        || bitmap.bitsPerPixel() != 32
        || !bitmap.hasAlpha()
        || bitmap.isPlanar()
        || bitmap.bitmapFormat() != NSBitmapFormat::empty()
        || bitmap.bytesPerRow() != ICON_ROW as isize
        || bitmap.bytesPerPlane() != ICON_RGBA_BYTES as isize
        || bitmap.bitmapData().is_null()
    {
        return None;
    }
    // SAFETY: This is our exclusive 4096-byte bitmap, checked above. Initialize
    // the entire canvas so transparent margins never expose uninitialized data.
    unsafe { std::ptr::write_bytes(bitmap.bitmapData(), 0, ICON_RGBA_BYTES) };
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)?;
    {
        NSGraphicsContext::saveGraphicsState_class();
        let _restore = GraphicsState;
        NSGraphicsContext::setCurrentContext(Some(&context));
        context.setImageInterpolation(NSImageInterpolation::High);
        let scale = (ICON_SIDE as f64 / source.width).min(ICON_SIDE as f64 / source.height);
        let width = source.width * scale;
        let height = source.height * scale;
        icon.drawInRect_fromRect_operation_fraction(
            NSRect::new(
                NSPoint::new(
                    (ICON_SIDE as f64 - width) / 2.0,
                    (ICON_SIDE as f64 - height) / 2.0,
                ),
                NSSize::new(width, height),
            ),
            NSRect::new(NSPoint::new(0.0, 0.0), source),
            NSCompositingOperation::Copy,
            1.0,
        );
        context.flushGraphics();
    }
    // SAFETY: Drawing is finished, the retained bitmap still owns this exact
    // allocation, and no code mutates it while the temporary slice is read.
    let pixels = unsafe { std::slice::from_raw_parts(bitmap.bitmapData(), ICON_RGBA_BYTES) };
    let png = encode_icon_png(pixels)?;
    Some(
        NSData::with_bytes(&png)
            .base64EncodedStringWithOptions(NSDataBase64EncodingOptions::empty())
            .to_string(),
    )
}

/// Fixed-size, metadata-free RGBA PNG. One stored DEFLATE block avoids an image
/// encoder accepting profiles/text or allocating in proportion to the source.
fn encode_icon_png(premultiplied_rgba: &[u8]) -> Option<Vec<u8>> {
    if premultiplied_rgba.len() != ICON_RGBA_BYTES {
        return None;
    }
    let mut scanlines = Vec::with_capacity(ICON_RGBA_BYTES + ICON_SIDE);
    for row in premultiplied_rgba.chunks_exact(ICON_ROW) {
        scanlines.push(0); // PNG filter: None
        for pixel in row.chunks_exact(4) {
            let alpha = u32::from(pixel[3]);
            for channel in &pixel[..3] {
                scanlines.push(if alpha == 0 {
                    0
                } else {
                    ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8
                });
            }
            scanlines.push(pixel[3]);
        }
    }
    let length = u16::try_from(scanlines.len()).ok()?;
    let mut compressed = vec![0x78, 0x01, 0x01]; // zlib, final stored block
    compressed.extend_from_slice(&length.to_le_bytes());
    compressed.extend_from_slice(&(!length).to_le_bytes());
    compressed.extend_from_slice(&scanlines);
    let (mut a, mut b) = (1_u32, 0_u32);
    for byte in &scanlines {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    compressed.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::from((ICON_SIDE as u32).to_be_bytes());
    header.extend_from_slice(&(ICON_SIDE as u32).to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    png_chunk(&mut png, b"IHDR", &header);
    png_chunk(&mut png, b"IDAT", &compressed);
    png_chunk(&mut png, b"IEND", &[]);
    (png.len() <= MAX_ICON_BYTES).then_some(png)
}

fn png_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = output.len();
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    let mut crc = u32::MAX;
    for byte in &output[start..] {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    output.extend_from_slice(&(!crc).to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_presence_never_filters_on_dock_or_frontmost_policy() {
        // A source-level boundary guard also covers the native collection path:
        // there must be no presentation-policy read capable of excluding a
        // pinned app when it changes from Regular to Accessory or Prohibited.
        let implementation = include_str!("macos_observer.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(!implementation.contains(".activationPolicy()"));
        assert!(!implementation.contains(".isActive()"));
    }

    #[test]
    fn launch_identity_is_exact_bounded_and_never_nan() {
        for value in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(launch_stamp(value).is_none());
        }
        let seconds = 1_700_000_000.123_456_f64;
        let stamp = launch_stamp(seconds).unwrap();
        assert_eq!(stamp.len(), 16);
        assert!(stamp
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_eq!(u64::from_str_radix(&stamp, 16).unwrap(), seconds.to_bits());
        assert_ne!(
            stamp,
            launch_stamp(f64::from_bits(seconds.to_bits() + 1)).unwrap()
        );
    }

    #[test]
    fn paths_require_bounded_local_absolute_locations() {
        assert!(is_absolute_local_path("/Applications/Example.app"));
        for path in [
            "",
            "Example.app",
            "//remote/Example.app",
            "file:///Applications/X.app",
            "/Applications/../X.app",
            "/Applications/./X.app",
            "/Applications/Bad\n.app",
        ] {
            assert!(!is_absolute_local_path(path), "{path:?}");
        }
        assert!(!is_absolute_local_path(&format!(
            "/{}",
            "a".repeat(MAX_PATH_BYTES)
        )));
        assert!(is_bundle_path(Path::new("/Applications/Example.app")));
        assert!(!is_bundle_path(Path::new(
            "/Applications/Example.app/Contents"
        )));
        assert!(!is_bundle_path(Path::new("/Applications/Example")));
    }

    #[test]
    fn display_labels_preserve_utf8_bounds() {
        let label = display_label(&"应\n用".repeat(200));
        assert!(label.len() <= MAX_DISPLAY_BYTES);
        assert!(!label.chars().any(char::is_control));
        assert!(!label.is_empty());
    }

    #[test]
    fn png_is_fixed_rgba_and_contains_no_source_metadata() {
        assert!(encode_icon_png(&[]).is_none());
        assert!(encode_icon_png(&[0; ICON_RGBA_BYTES + 1]).is_none());
        let mut pixels = [0; ICON_RGBA_BYTES];
        pixels[..4].copy_from_slice(&[64, 32, 0, 128]);
        let png = encode_icon_png(&pixels).unwrap();
        assert_eq!(png.len(), 4196);
        assert!(png.len() < MAX_ICON_BYTES);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..29], &[0, 0, 0, 32, 0, 0, 0, 32, 8, 6, 0, 0, 0]);
        assert_eq!(&png[37..41], b"IDAT");
        assert_eq!(&png[41..44], &[0x78, 0x01, 0x01]);
        let length = u16::from_le_bytes(png[44..46].try_into().unwrap());
        assert_eq!(length as usize, ICON_RGBA_BYTES + ICON_SIDE);
        assert_eq!(u16::from_le_bytes(png[46..48].try_into().unwrap()), !length);
        assert_eq!(&png[48..53], &[0, 128, 64, 0, 128]);
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        // Standard, fixed IEND CRC also exercises the CRC32 implementation.
        assert_eq!(&png[png.len() - 4..], &[0xae, 0x42, 0x60, 0x82]);
    }

    #[test]
    fn reused_pid_or_changed_native_location_is_not_the_same_identity() {
        let original = Identity {
            pid: 123,
            process: ProcessIdentity {
                uid: 501,
                started_sec: 1_700_000_000,
                started_usec: 123,
            },
            started: launch_stamp(1_700_000_000.0).unwrap(),
            bundle_path: "/Applications/Example.app".into(),
            bundle_id: Some("org.example.App".into()),
            executable_url: Some("/Applications/Example.app/Contents/MacOS/Example".into()),
            executable_path: Some("/Applications/Example.app/Contents/MacOS/Example".into()),
        };
        let mut changed = original.clone();
        changed.process.started_usec += 1;
        assert_ne!(changed, original);
        let mut changed = original.clone();
        changed.started = launch_stamp(1_700_000_001.0).unwrap();
        assert_ne!(changed, original);
        let mut changed = original.clone();
        changed.bundle_path = "/Applications/Other.app".into();
        assert_ne!(changed, original);
        let mut changed = original.clone();
        changed.executable_url = None;
        assert_ne!(changed, original);
        let mut changed = original.clone();
        changed.executable_path = None;
        assert_ne!(changed, original);
    }

    #[test]
    fn executable_must_be_inside_the_canonical_bundle() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("vibdock-observer-{}-{nonce}", std::process::id()));
        let bundle = directory.join("Example.app");
        let bin = bundle.join("Contents/MacOS/Example");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"test fixture, never executed").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let bundle = bundle.canonicalize().unwrap();
        let bin = bin.canonicalize().unwrap();
        assert!(is_bundle_executable(&bundle, &bin));
        assert!(!is_bundle_executable(
            &bundle,
            &bundle.join("Contents/MacOS")
        ));
        let outside = directory.join("outside");
        std::fs::write(&outside, b"test fixture, never executed").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&outside, bundle.join("Contents/MacOS/Link")).unwrap();
        let linked = bundle.join("Contents/MacOS/Link").canonicalize().unwrap();
        assert!(!is_bundle_executable(&bundle, &linked));
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!is_bundle_executable(&bundle, &bin));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn libproc_abi_and_invalid_pids_fail_closed() {
        assert_eq!(std::mem::size_of::<BsdInfo>(), 136);
        assert_eq!(std::mem::align_of::<BsdInfo>(), 8);
        assert_eq!(std::mem::offset_of!(BsdInfo, started_sec), 120);
        assert_eq!(std::mem::offset_of!(BsdInfo, started_usec), 128);
        assert!(process_identity(0).is_none());
        assert!(process_identity(-1).is_none());
        assert!(process_executable(0).is_none());
        assert!(process_executable(-1).is_none());
        let pid = std::process::id() as i32;
        let own = process_identity(pid).expect("own process identity is readable");
        // SAFETY: The no-argument libc read has no side effects.
        assert_eq!(own.uid, unsafe { getuid() });
        assert_eq!(
            process_executable(pid),
            std::env::current_exe().unwrap().canonicalize().ok()
        );
    }

    #[test]
    fn background_thread_is_unavailable_before_appkit_work() {
        let snapshot = std::thread::spawn(snapshot).join().unwrap();
        assert_eq!(snapshot["available"], false);
        assert_eq!(snapshot["complete"], false);
        assert_eq!(snapshot["applications"], json!([]));
    }
}
