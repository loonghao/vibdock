#!/usr/bin/env python3
"""Package existing native binaries; no compiler, installer, signing or download.

The .app is an unsigned/unnotarized candidate. The build toolchain's local
ad-hoc Mach-O signature, if present, is not a Developer ID signature.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import struct
import zipfile


def safe_segment(value):
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,79}", value) or ".." in value:
        raise ValueError("Package tag must be one safe filename segment")
    return value


def check_macho(path, target):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 20 * 1024 * 1024:
        raise ValueError("Missing, symlinked or oversized native executable")
    with path.open("rb") as source:
        header = source.read(32)
    if len(header) != 32 or header[:4] != b"\xcf\xfa\xed\xfe":
        raise ValueError("Expected a native little-endian Mach-O 64-bit executable")
    cpu, _, kind = struct.unpack_from("<III", header, 4)
    expected = {"aarch64-apple-darwin": 0x0100000C, "x86_64-apple-darwin": 0x01000007}
    if cpu != expected[target] or kind != 2 or not os.access(path, os.X_OK):
        raise ValueError("Executable architecture/type/execute bit does not match the bundle")


def check_brand(brand):
    hashes = json.loads((brand / "SHA256.json").read_text())
    for name, expected in hashes.items():
        safe_segment(name)
        if hashlib.sha256((brand / name).read_bytes()).hexdigest() != expected:
            raise ValueError("Original brand content hash mismatch")
    # ICNS is only a container: every PNG is byte-for-byte from the owner's pack.
    payload = (brand / "vibdock.icns").read_bytes()
    if payload[:4] != b"icns" or struct.unpack_from(">I", payload, 4)[0] != len(payload):
        raise ValueError("Invalid original-art ICNS container")
    sizes = {b"icp4": 16, b"icp5": 32, b"icp6": 64, b"ic07": 128,
             b"ic08": 256, b"ic09": 512, b"ic10": 1024}
    source_hashes = json.loads((brand / "MACOS-PNG-SHA256.json").read_text())
    offset, seen = 8, set()
    while offset < len(payload):
        kind, length = struct.unpack_from(">4sI", payload, offset)
        if kind not in sizes or kind in seen or length < 32 or offset + length > len(payload):
            raise ValueError("Invalid or repeated ICNS image")
        png = payload[offset + 8:offset + length]
        size = sizes[kind]
        if png[:8] != b"\x89PNG\r\n\x1a\n" or struct.unpack_from(">II", png, 16) != (size, size):
            raise ValueError("Invalid ICNS PNG dimensions")
        if hashlib.sha256(png).hexdigest() != source_hashes[str(size)]:
            raise ValueError("ICNS image is not the original supplied PNG")
        seen.add(kind)
        offset += length
    if seen != set(sizes):
        raise ValueError("Incomplete original-art ICNS")


def package(root, binary_directory, output, tag, target):
    safe_segment(tag)
    if target not in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
        raise ValueError("Unsupported macOS target")
    root, binary_directory, output = root.resolve(), binary_directory.resolve(), output.absolute()
    for name in ("README.md", "LICENSE", "Cargo.toml", "assets", "assets/brand", "docs"):
        if (root / name).is_symlink():
            raise ValueError("Top-level package inputs must not be symlinks")
    for name in ("assets", "docs"):
        for source in (root / name).rglob("*"):
            if source.is_symlink():
                raise ValueError("Package resources must not follow symlinks")
    for name in ("vibdock", "vibdock-launcher"):
        check_macho(binary_directory / name, target)
    check_brand(root / "assets/brand")
    version_match = re.search(r'^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"',
                              (root / "Cargo.toml").read_text(), re.MULTILINE)
    if not version_match:
        raise ValueError("Missing stable package version")
    # Refuse an existing destination rather than replacing somebody's .app.
    output.mkdir(parents=True, exist_ok=False)
    app = output / "VibDock.app"
    macos = app / "Contents/MacOS"
    resources = app / "Contents/Resources"
    macos.mkdir(parents=True)
    resources.mkdir()
    for name in ("vibdock", "vibdock-launcher"):
        shutil.copy2(binary_directory / name, macos / name)
    for name in ("README.md", "LICENSE"):
        shutil.copy2(root / name, resources / name)
    for name in ("assets", "docs"):
        shutil.copytree(root / name, resources / name, symlinks=False)
    shutil.copy2(root / "assets/brand/vibdock.icns", resources / "vibdock.icns")
    metadata = {
        "CFBundleIdentifier": "com.loonghao.vibdock",
        "CFBundleName": "VibDock", "CFBundleDisplayName": "VibDock",
        "CFBundleExecutable": "vibdock-launcher", "CFBundlePackageType": "APPL",
        "CFBundleInfoDictionaryVersion": "6.0", "CFBundleIconFile": "vibdock.icns",
        "CFBundleShortVersionString": version_match[1], "CFBundleVersion": version_match[1],
        "NSPrincipalClass": "NSApplication", "NSHighResolutionCapable": True,
        "LSMinimumSystemVersion": "11.0", "LSUIElement": False,
        "VibDockSourceRevision": os.environ.get("VIBDOCK_SOURCE_REVISION", "unknown"),
        "VibDockCandidateTag": tag,
    }
    # Do not use LSMultipleInstancesProhibited: it forbids different users from
    # running concurrently. Our launcher owns a per-user advisory file lock.
    (app / "Contents/Info.plist").write_bytes(plistlib.dumps(metadata, sort_keys=True))
    archive = output / f"vibdock-{tag}-{target}.zip"
    with zipfile.ZipFile(archive, "x", compression=zipfile.ZIP_DEFLATED) as result:
        for source in sorted(app.rglob("*")):
            if source.is_symlink():
                raise ValueError("Bundle must not contain symlinks")
            if source.is_file():
                result.write(source, source.relative_to(output))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_suffix(".zip.sha256").write_text(f"{digest}  {archive.name}\n")
    return {"app": str(app), "archive": str(archive), "sha256": digest,
            "signedByDeveloperID": False, "notarized": False}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--binary-directory", type=Path, default=Path("target/release"))
    parser.add_argument("--output-directory", type=Path, default=Path("work/macos-package"))
    parser.add_argument("--tag", required=True)
    parser.add_argument("--target", choices=["aarch64-apple-darwin", "x86_64-apple-darwin"], required=True)
    args = parser.parse_args()
    print(json.dumps(package(args.root, args.binary_directory, args.output_directory,
                             args.tag, args.target)))
