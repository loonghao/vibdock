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

REQUIRED = ["application_finished_launching", "service_ownership_proved", "native_panel_visible", "floating_panel",
            "original_template_image", "retained_status_item", "dock_entry",
            "status_menu", "dock_menu", "hide_keeps_resident", "dock_show_reuses_panel",
            "close_hides_only", "reopen_reuses_panel", "last_window_keeps_resident",
            "settings_handoff_issued", "authenticated_shutdown_and_port_released",
            "native_view_rendered", "native_view_png_written", "native_resources_released",
            "second_launcher_exited", "owner_ipc_reopen_restores"]


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
               VIBDOCK_OVERLAY="0",
               VIBDOCK_NATIVE_ACCEPTANCE_SYSTEM_QUIT="1" if system_quit else "0")
    config = state / "VibDock/config.json"
    config.parent.mkdir()
    original = b'{"device_id":null,"hotkey":null}\n'
    config.write_bytes(original)
    result = None
    try:
        # The launcher starts/proves its own service. The same real UI worker
        # issues Settings handoff and authenticates Quit; no routes are mocked.
        try:
            result = subprocess.run([str(launcher), "--macos-native-acceptance"], env=env,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    timeout=40, check=False, text=True)
        except subprocess.TimeoutExpired as error:
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
        for key in REQUIRED + ["configPreserved", "ownedIpcSocketRemoved", "launcherLockReleased"]:
            if report.get(key) is not True:
                raise RuntimeError(f"Native acceptance missing required check: {key}")
        if report.get("browser_launched") is not False or report.get("audio_supported") is not False:
            raise RuntimeError("Acceptance exceeded its browser/audio scope")
        png = image.read_bytes()
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
