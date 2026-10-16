#!/usr/bin/env python3
"""Exercise the actual AppKit entry in isolated state, without TCC permissions.

The PNG is the application's own native view bitmap, not a screenshot of the
desktop or menu bar. This acceptance never launches a default browser or audio.
"""
import argparse
import errno
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import socket
import struct
import subprocess
import tempfile
import time

REQUIRED = ["application_finished_launching", "service_ownership_proved", "native_panel_visible", "floating_panel",
            "original_template_image", "retained_status_item", "dock_entry",
            "status_menu", "dock_menu", "hide_keeps_resident", "dock_show_reuses_panel",
            "close_hides_only", "reopen_reuses_panel", "last_window_keeps_resident",
            "settings_handoff_issued", "authenticated_shutdown_and_port_released",
            "native_view_rendered", "native_view_png_written", "native_resources_released",
            "native_view_opaque_root", "native_view_background_opaque",
            "native_light_view_rendered", "native_dark_view_rendered",
            "native_light_background_opaque", "native_dark_background_opaque",
            "native_appearance_backgrounds_distinct",
            "second_launcher_exited", "owner_ipc_reopen_restores"]
SOFTWARE_REQUIRED = ["software_rows_match_summary", "software_rows_within_panel",
                     "software_status_menu_rows", "software_dock_menu_rows",
                     "software_presence_only", "software_expiry_unknown",
                     "software_expiry_clears_icons", "software_persisted_offline_label",
                     "software_offline_not_running_or_unknown", "software_offline_placeholder",
                     "software_self_running_observed", "software_self_native_icon",
                     "software_self_brand_icon_colors"]
OBSERVER_COUNTERS = ("enumerated", "inspected", "other_user_excluded", "unreadable_process",
                     "invalid_application_identity", "changed_before_publish", "missing_icons",
                     "collection_limited", "publication_budget_expired", "missing_launch_date")
IDENTITY_STAGES = ("terminated", "unreadable_process", "other_user", "missing_launch_date",
                   "invalid_launch_date", "missing_bundle_url", "canonical_bundle",
                   "invalid_bundle_identifier", "canonical_executable", "executable_mismatch",
                   "native_changed", "missing_current_application", "changed_application_object",
                   "changed_identity")


def safe_counters(value, names):
    if not isinstance(value, dict):
        return {}
    return {key: value[key] for key in names
            if type(value.get(key)) is int and 0 <= value[key] <= 1024}


def diagnostic_text(value):
    # This subprocess uses fresh synthetic state only. Still remove URLs,
    # capabilities and filesystem paths before publishing bounded diagnostics.
    value = re.sub(r"https?://[^\s\"']+", "<url>", value)
    value = re.sub(r"[A-Fa-f0-9]{24,}", "<redacted>", value)
    value = re.sub(r"(?:[A-Za-z]:)?/[^\s\"']+", "<path>", value)
    return value[:2048]


def port_is_closed(port):
    with socket.socket() as probe:
        probe.settimeout(0.3)
        return probe.connect_ex(("127.0.0.1", port)) == errno.ECONNREFUSED


def verify_helper_lifecycle(cli, env):
    # Execute only the packaged helper, in the isolated synthetic state. No
    # application identity, icon bytes or local paths are copied into reports.
    child = subprocess.Popen([str(cli), "_resident_snapshot"], env=env,
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    started = time.monotonic()
    try:
        # Hold stdin open but withhold activation. The helper's own hard
        # watchdog must terminate even without its normal parent owner.
        code = child.wait(timeout=7)
        if code != 125 or not 3 <= time.monotonic() - started < 7:
            raise RuntimeError("Native observer watchdog did not enforce its deadline")
    finally:
        if child.poll() is None:
            child.kill()
        child.communicate(timeout=2)
    child = subprocess.Popen([str(cli), "_resident_snapshot"], env=env,
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        child.stdin.write(b"V")
        child.stdin.flush()
        child.stdin.close()
        child.stdin = None
        code = child.wait(timeout=3)
        output, _ = child.communicate(timeout=2)
        if code not in (0, 125) or len(output) > 512 * 1024:
            raise RuntimeError("Native observer parent-EOF cleanup was not confirmed")
    finally:
        if child.poll() is None:
            child.kill()
            child.communicate(timeout=2)
    return {"observerHardWatchdog": True, "observerParentEofCleanup": True}


def observe_diagnostics(cli, env, app, launcher_pid):
    """Observe a slow native run without exposing the helper's private inventory."""
    with tempfile.TemporaryFile() as output:
        child = subprocess.Popen([str(cli), "_resident_snapshot"], env=env,
                                 stdin=subprocess.PIPE, stdout=output, stderr=subprocess.DEVNULL)
        try:
            child.stdin.write(b"V")
            child.stdin.flush()
            try:
                code = child.wait(timeout=3.5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=1)
                return {"completed": False, "reason": "probe deadline"}
            output.seek(0)
            raw = output.read(512 * 1024 + 1)
            if code != 0 or len(raw) > 512 * 1024:
                return {"completed": False, "reason": "probe exit or output bound", "exitCode": code}
            try:
                value = json.loads(raw)
                apps = value.get("applications", [])
                return {"completed": True, "available": value.get("available") is True,
                        "complete": value.get("complete") is True,
                        "applicationCount": len(apps),
                        "nativeIconCount": sum(isinstance(a.get("icon_b64"), str) for a in apps),
                        "missingExecutableCount": sum(not isinstance(a.get("executable_path"), str) for a in apps),
                        "bundledApplicationObserved": any(a.get("bundle_path") == str(app) for a in apps),
                        "launcherProcessObserved": any(a.get("process_id") == launcher_pid for a in apps),
                        "counts": safe_counters(value.get("diagnostics"), OBSERVER_COUNTERS),
                        "identityStages": safe_counters(value.get("diagnostics", {}).get("identity_stages"), IDENTITY_STAGES)}
            except (ValueError, TypeError, AttributeError):
                return {"completed": False, "reason": "invalid probe output"}
        finally:
            child.stdin.close()
            if child.poll() is None:
                child.kill()
                child.wait(timeout=1)


def run(app, evidence, system_quit=False):
    if platform.system() != "Darwin":
        raise RuntimeError("Native macOS acceptance requires macOS")
    import fcntl
    app, evidence = app.resolve(), evidence.resolve()
    launcher = app / "Contents/MacOS/vibdock-launcher"
    cli = app / "Contents/MacOS/vibdock"
    for binary in (launcher, cli):
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise RuntimeError("Missing native bundle executable")
    evidence.mkdir(parents=True, exist_ok=True)
    suffix = "-system-quit" if system_quit else ""
    image = evidence / f"resident-native-view{suffix}.png"
    # A fresh state root, not HOME and not an existing user service record.
    state = Path(tempfile.mkdtemp(prefix="vibdock-macos-desktop-")).resolve()
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    # Python and Darwin Rust have different fallback temporary roots when
    # TMPDIR is absent. Give the child the exact parent we just selected;
    # the launcher still validates canonical containment and a fresh record.
    env = dict(os.environ, TMPDIR=str(state.parent), LOCALAPPDATA=str(state), VIBDOCK_PORT=str(port),
               VIBDOCK_NATIVE_ACCEPTANCE="1", VIBDOCK_NATIVE_ACCEPTANCE_IMAGE=str(image),
               VIBDOCK_NATIVE_ACCEPTANCE_SOFTWARE_ID="b" * 64,
               VIBDOCK_NATIVE_ACCEPTANCE_SOFTWARE_LABEL="Offline Mac software fixture",
               VIBDOCK_NATIVE_ACCEPTANCE_RUNNING_ID="c" * 64,
               VIBDOCK_OVERLAY="0",
               VIBDOCK_NATIVE_ACCEPTANCE_SYSTEM_QUIT="1" if system_quit else "0")
    config = state / "VibDock/config.json"
    config.parent.mkdir()
    original = b'{"device_id":null,"hotkey":null}\n'
    config.write_bytes(original)
    # One explicitly offline saved location, and the actual bundled app whose
    # launch identity and icon must be independently observed by the helper.
    # These fixtures never pretend to be audio/playback or application work.
    software = state / "VibDock/apps-macos.json"
    saved = json.dumps({"schema_version": 1, "platform": "macos", "revision": 1,
                        "entries": [
                            {"id": "b" * 64, "label": env["VIBDOCK_NATIVE_ACCEPTANCE_SOFTWARE_LABEL"],
                             "bundle_path": "/Applications/VibDockFixture-NeverLaunched.app",
                             "bundle_id": "invalid.vibdock.fixture"},
                            {"id": "c" * 64, "label": "VibDock acceptance application",
                             "bundle_path": str(app), "bundle_id": "com.loonghao.vibdock"}]}).encode()
    software.write_bytes(saved)
    windows_software = state / "VibDock/apps.json"
    windows_original = b'{"windows-store-must-remain-untouched":true}\n'
    windows_software.write_bytes(windows_original)
    result = None
    try:
        helper_checks = verify_helper_lifecycle(cli, env)
        # The launcher starts/proves its own service. The same real UI worker
        # issues Settings handoff and authenticates Quit; no routes are mocked.
        try:
            child = subprocess.Popen([str(launcher), "--macos-native-acceptance"], env=env,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                stdout, stderr = child.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                helper_checks["slowRunObserverProbe"] = observe_diagnostics(cli, env, app, child.pid)
                stdout, stderr = child.communicate(timeout=31)
            result = subprocess.CompletedProcess(child.args, child.returncode, stdout, stderr)
        except subprocess.TimeoutExpired as error:
            child.kill()
            child.communicate(timeout=2)
            stderr = error.stderr or b""
            if isinstance(stderr, bytes):
                stderr = stderr.decode("utf-8", errors="replace")
            diagnostic = {"passed": False, "stage": "native-entry-timeout",
                          "system_termination_path": system_quit,
                          "stderr": diagnostic_text(stderr),
                          "stdoutBytes": len(error.stdout or b""),
                          "privatePortClosed": port_is_closed(port)}
            (evidence / f"report{suffix}.json").write_text(json.dumps(diagnostic, indent=2) + "\n")
            raise

        reports = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
        if len(reports) != 1:
            diagnostic = {"passed": False, "stage": "native-entry-report",
                          "launcherReturnCode": result.returncode,
                          "stderr": diagnostic_text(result.stderr),
                          "stdoutBytes": len(result.stdout.encode()),
                          "serviceRecordPresent": (state / "VibDock/service.json").exists(),
                          "privatePortClosed": port_is_closed(port)}
            (evidence / f"report{suffix}.json").write_text(json.dumps(diagnostic, indent=2) + "\n")
            raise RuntimeError(f"Native entry did not emit an acceptance report (exit {result.returncode}); inspect redacted report")
        report = reports[0]
        report.update(helper_checks)
        lock_path = state / "VibDock/macos-launcher.lock"
        # This local-only read is a Show capability, never a service token.
        # Do not copy either the socket name or nonce into public evidence.
        record = json.loads(lock_path.read_text())
        name = record["socket"]
        if len(name) != 64 or any(c not in "0123456789abcdef" for c in name):
            raise RuntimeError("Invalid private owner IPC record")
        ipc_gone = not (Path("/tmp") / f"vibdock-ui-{name}.sock").exists()
        with lock_path.open("rb") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            fcntl.flock(lock, fcntl.LOCK_UN)
        report.update(sourceRevision=os.environ.get("VIBDOCK_SOURCE_REVISION"),
                      launcherSHA256=hashlib.sha256(launcher.read_bytes()).hexdigest(),
                      platform="macOS", mockedRoutes=False,
                      configPreserved=config.read_bytes() == original,
                      softwarePinsPreserved=software.read_bytes() == saved,
                      windowsSoftwareStoreUntouched=windows_software.read_bytes() == windows_original,
                      ownedIpcSocketRemoved=ipc_gone, launcherLockReleased=True,
                      entry="actual bundled Mach-O launcher",
                      exclusions=["physical pointer/focus and full-desktop menu-bar capture",
                                  "default-browser launch", "live application audio/voice state",
                                  "Developer ID signing and notarization"])
        (evidence / f"report{suffix}.json").write_text(json.dumps(report, indent=2) + "\n")
        if result.returncode != 0 or report.get("passed") is not True:
            raise RuntimeError("Native acceptance failed; inspect the redacted report")
        if report.get("system_termination_path") is not system_quit:
            raise RuntimeError("Native acceptance exercised the wrong termination path")
        if system_quit and report.get("system_termination_cleaned") is not True:
            raise RuntimeError("System termination did not report pre-exit native cleanup")
        if system_quit and report.get("system_termination_trigger_deferred") is not True:
            raise RuntimeError("System termination was not requested outside the worker poll callback")
        for key in REQUIRED + SOFTWARE_REQUIRED + ["configPreserved", "softwarePinsPreserved",
                "windowsSoftwareStoreUntouched", "ownedIpcSocketRemoved", "launcherLockReleased"]:
            if report.get(key) is not True:
                raise RuntimeError(f"Native acceptance missing required check: {key}")
        if report.get("browser_launched") is not False or report.get("audio_supported") is not False:
            raise RuntimeError("Acceptance exceeded its browser/audio scope")
        for appearance in (None, "light", "dark"):
            capture = image if appearance is None else image.with_name(
                f"{image.stem}-{appearance}.png")
            png = capture.read_bytes()
            if len(png) < 1024 or png[:8] != b"\x89PNG\r\n\x1a\n":
                raise RuntimeError("Native view PNG was not rendered")
            width, height = struct.unpack_from(">II", png, 16)
            if not (100 <= width <= 2400 and 60 <= height <= 1600):
                raise RuntimeError("Unexpected native view image bounds")
        if (state / "VibDock/service.json").exists():
            raise RuntimeError("Quit left the test service record behind")
        print("PASS: actual AppKit widgets, owner-specific singleton reopen, proved Settings handoff, authenticated Quit and original-art native view rendering.")
    finally:
        # Recovery addresses only this isolated service through the existing
        # proof protocol. Never use killall, a process-name match, or a token log.
        marker = state / "VibDock/service.json"
        if marker.exists():
            stop = subprocess.run([str(cli), "service-stop"], env=env, timeout=8,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
            if stop.returncode != 0:
                raise RuntimeError("Isolated service cleanup unconfirmed; private test state retained")
        # An already successful authenticated Quit has no service left to stop.
        # Absence alone is insufficient: the isolated listening port must close.
        if marker.exists() or not port_is_closed(port):
            raise RuntimeError("Isolated service cleanup unconfirmed; private test state retained")
        shutil.rmtree(state)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, default=Path("work/macos-package/VibDock.app"))
    parser.add_argument("--evidence", type=Path, default=Path("work/macos-desktop"))
    args = parser.parse_args()
    run(args.app, args.evidence)
    run(args.app, args.evidence, system_quit=True)
