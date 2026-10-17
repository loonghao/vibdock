#!/usr/bin/env python3
"""Dev-only SenseVoice transcription prototype for issue #44.

This script is not a VibDock runtime dependency and is not wired into
vibdock.exe. It proves that an existing local SenseVoice ONNX model can
transcribe a WAV file offline, and documents the explicit-download contract
that a future native helper would have to implement.

The script never uploads audio. It reads a WAV file, downmixes/resamples it to
16 kHz in memory, runs sherpa-onnx locally, and prints one JSON object.

Exit codes:
    0  success
    1  audio could not be read, inference failed, or download/verification failed
    2  invalid arguments (including a download without explicit consent)
    3  a required Python dependency is missing
    4  no usable model was found in any discovery location
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import wave
from math import gcd
from pathlib import Path

try:
    import numpy as np
    import sherpa_onnx
    from scipy.signal import resample_poly
except ImportError as exc:  # pragma: no cover - depends on the host environment
    sys.stderr.write(
        "error: missing Python dependency "
        + (exc.name or "unknown")
        + "\n"
        + "install the dev-only prototype dependencies with:\n"
        + "  python -m pip install sherpa-onnx numpy scipy\n"
    )
    raise SystemExit(3) from exc

SAMPLE_RATE = 16_000
MODEL_FILE_NAMES = ("model.int8.onnx", "model.onnx")
TOKENS_FILE_NAME = "tokens.txt"

# Pinned official source. Values were read from the GitHub release asset API for
# k2-fsa/sherpa-onnx tag "asr-models" and are verified again after download.
DOWNLOAD_URL = (
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/"
    "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2"
)
DOWNLOAD_SHA256 = "7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e"
DOWNLOAD_SIZE = 163_002_883
LICENSE_URL = "https://github.com/FunAudioLLM/SenseVoice/blob/main/LICENSE"
MODEL_REFERENCE_URL = (
    "https://k2-fsa.github.io/sherpa/onnx/sense-voice/pretrained.html"
)


class ModelNotFound(RuntimeError):
    """Raised when no usable local SenseVoice model can be discovered."""


class DownloadError(RuntimeError):
    """Raised when the explicit, checksum-verified download cannot complete."""


def dsh_home() -> Path:
    """Return the DeepSeek Harness home directory without assuming a username."""
    configured = os.environ.get("DSH_HOME")
    if configured:
        return Path(configured).expanduser()
    return Path.home() / ".dsh"


def dsh_model_dir() -> Path:
    """Existing DSH speech-to-text cache; shared read-only, never modified here."""
    return dsh_home() / "speech-to-text" / "sensevoice" / "models" / "sensevoice-onnx"


def vibdock_model_dir() -> Path:
    """VibDock-owned model directory used only by an explicit install."""
    local_app_data = os.environ.get("LOCALAPPDATA")
    if local_app_data:
        return Path(local_app_data) / "VibDock" / "models" / "sensevoice"
    return Path.home() / "AppData" / "Local" / "VibDock" / "models" / "sensevoice"


def find_model_files(directory: Path) -> tuple[Path, Path] | None:
    """Return (model, tokens) when the directory looks like a SenseVoice model."""
    if not directory.is_dir():
        return None
    model = next(
        (directory / name for name in MODEL_FILE_NAMES if (directory / name).is_file()),
        None,
    )
    tokens = directory / TOKENS_FILE_NAME
    if model is None or not tokens.is_file():
        return None
    if model.stat().st_size <= 0 or tokens.stat().st_size <= 0:
        return None
    return model, tokens


def discovery_candidates() -> list[tuple[str, Path]]:
    """Discovery order: existing DSH cache first, then the VibDock model folder."""
    return [
        ("DSH SenseVoice cache", dsh_model_dir()),
        ("VibDock model directory", vibdock_model_dir()),
    ]


def _model_guidance() -> str:
    return (
        "Options:\n"
        "  1. Use a model that is already on this machine:\n"
        "       python scripts/stt/sensevoice_transcribe.py <wav> --model-dir <dir>\n"
        "  2. Install the pinned INT8 model explicitly over HTTPS, then verify it:\n"
        "       python scripts/stt/sensevoice_transcribe.py --download --consent-download\n"
        f"     source: {DOWNLOAD_URL}\n"
        f"     size: {DOWNLOAD_SIZE:,} bytes\n"
        f"     sha256: {DOWNLOAD_SHA256}\n"
        f"     license notes: {LICENSE_URL}\n"
        "No model is ever downloaded silently.\n"
    )


def resolve_model_dir(explicit: str | None) -> Path:
    """Resolve the model directory.

    An explicit ``--model-dir`` is a hard override: if it does not contain a
    usable model the script fails closed instead of silently transcribing with
    a different local model.
    """
    if explicit:
        directory = Path(explicit).expanduser()
        if find_model_files(directory) is None:
            raise ModelNotFound(
                "error: --model-dir does not contain a usable SenseVoice model.\n"
                f"  checked: {directory}\n"
                f"  expected: one of {', '.join(MODEL_FILE_NAMES)} plus {TOKENS_FILE_NAME}\n"
                "\n"
                + _model_guidance()
            )
        return directory

    searched = []
    for label, directory in discovery_candidates():
        searched.append(f"  - {directory} ({label})")
        if find_model_files(directory) is not None:
            return directory

    raise ModelNotFound(
        "error: no usable local SenseVoice model was found.\n"
        "\n"
        "Looked for model.int8.onnx (or model.onnx) plus tokens.txt in:\n"
        + "\n".join(searched)
        + "\n"
        "\n"
        + _model_guidance()
    )


def read_wav_16k_mono(path: Path) -> tuple[np.ndarray, float]:
    """Read a PCM WAV, downmix to mono, and resample to 16 kHz."""
    try:
        with wave.open(str(path), "rb") as wav:
            channels = wav.getnchannels()
            sample_width = wav.getsampwidth()
            source_rate = wav.getframerate()
            frames = wav.getnframes()
            raw = wav.readframes(frames)
    except (OSError, EOFError, wave.Error) as exc:
        raise RuntimeError(f"error: could not read WAV '{path}': {exc}") from exc

    if source_rate <= 0:
        raise RuntimeError(f"error: WAV '{path}' reports an invalid sample rate")
    if frames <= 0 or not raw:
        raise RuntimeError(f"error: WAV '{path}' contains no audio frames")

    if sample_width == 1:
        samples = (np.frombuffer(raw, dtype=np.uint8).astype(np.float32) - 128.0) / 128.0
    elif sample_width == 2:
        samples = np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0
    elif sample_width == 3:
        octets = np.frombuffer(raw, dtype=np.uint8).reshape(-1, 3).astype(np.int32)
        values = octets[:, 0] | (octets[:, 1] << 8) | (octets[:, 2] << 16)
        values = np.where(values >= (1 << 23), values - (1 << 24), values)
        samples = values.astype(np.float32) / float(1 << 23)
    elif sample_width == 4:
        samples = np.frombuffer(raw, dtype="<i4").astype(np.float32) / 2147483648.0
    else:
        raise RuntimeError(
            f"error: unsupported WAV sample width of {sample_width} bytes in '{path}'"
        )

    if channels > 1:
        samples = samples.reshape(-1, channels).mean(axis=1)

    if source_rate != SAMPLE_RATE:
        divisor = gcd(int(source_rate), SAMPLE_RATE)
        samples = resample_poly(
            samples, SAMPLE_RATE // divisor, int(source_rate) // divisor
        )

    samples = np.ascontiguousarray(samples, dtype=np.float32)
    if samples.size == 0:
        raise RuntimeError(f"error: WAV '{path}' produced no samples")
    return samples, frames / float(source_rate)


def transcribe(
    wav_path: Path,
    model_dir: Path,
    *,
    threads: int,
    language: str,
    use_itn: bool,
) -> dict[str, object]:
    files = find_model_files(model_dir)
    if files is None:
        raise ModelNotFound(f"error: model directory '{model_dir}' is not usable")
    model_path, tokens_path = files

    audio, audio_seconds = read_wav_16k_mono(wav_path)

    recognizer = sherpa_onnx.OfflineRecognizer.from_sense_voice(
        model=str(model_path),
        tokens=str(tokens_path),
        num_threads=threads,
        use_itn=use_itn,
        language=language,
    )

    started = time.perf_counter()
    stream = recognizer.create_stream()
    stream.accept_waveform(SAMPLE_RATE, audio)
    recognizer.decode_stream(stream)
    seconds = time.perf_counter() - started

    result = stream.result
    return {
        "text": (result.text or "").strip(),
        "lang": _strip_tag(result.lang),
        "emotion": _strip_tag(result.emotion),
        "event": _strip_tag(result.event),
        "audio_seconds": round(audio_seconds, 3),
        "seconds": round(seconds, 4),
    }


def _strip_tag(value: str | None) -> str | None:
    """SenseVoice returns tags such as '<|zh|>'; report the value inside."""
    if value is None:
        return None
    text = value.strip()
    if text.startswith("<|") and text.endswith("|>"):
        return text[2:-2]
    return text


def _download_archive(destination: Path) -> None:
    """Download the pinned archive over HTTPS, refusing anything unexpected."""
    parsed = urllib.parse.urlparse(DOWNLOAD_URL)
    if parsed.scheme != "https":
        raise DownloadError("error: refusing a non-HTTPS model download")

    request = urllib.request.Request(
        DOWNLOAD_URL,
        headers={"User-Agent": "vibdock-stt-prototype/1"},
    )
    written = 0
    digest = hashlib.sha256()
    with urllib.request.urlopen(request, timeout=60) as response:
        final_scheme = urllib.parse.urlparse(response.geturl()).scheme
        if final_scheme != "https":
            raise DownloadError("error: model download redirected away from HTTPS")
        with destination.open("wb") as handle:
            while True:
                chunk = response.read(1024 * 1024)
                if not chunk:
                    break
                written += len(chunk)
                if written > DOWNLOAD_SIZE:
                    raise DownloadError(
                        "error: downloaded model exceeds the pinned size; aborted"
                    )
                digest.update(chunk)
                handle.write(chunk)

    if written != DOWNLOAD_SIZE:
        raise DownloadError(
            f"error: downloaded {written} bytes, expected {DOWNLOAD_SIZE} bytes"
        )
    actual = digest.hexdigest()
    if actual != DOWNLOAD_SHA256:
        raise DownloadError(
            "error: model checksum mismatch; the archive was discarded.\n"
            f"  expected: {DOWNLOAD_SHA256}\n"
            f"  actual:   {actual}"
        )


def _extract_archive(archive: Path, destination: Path) -> None:
    """Extract a tar.bz2 archive after rejecting unsafe members."""
    root = destination.resolve()
    try:
        with tarfile.open(archive, "r:bz2") as tar:
            for member in tar.getmembers():
                target = (destination / member.name).resolve()
                if target != root and root not in target.parents:
                    raise DownloadError(
                        f"error: model archive contains an unsafe path: {member.name}"
                    )
                if member.issym() or member.islnk():
                    raise DownloadError(
                        f"error: model archive contains a link: {member.name}"
                    )
            try:
                tar.extractall(destination, filter="data")
            except TypeError:  # Python < 3.12
                tar.extractall(destination)
    except (tarfile.TarError, OSError) as exc:
        raise DownloadError(f"error: could not extract the model archive: {exc}") from exc


def _install_from_extracted(extracted: Path, target: Path) -> None:
    """Flatten the archive's single model directory into the target directory."""
    candidates = [extracted] + [entry for entry in extracted.iterdir() if entry.is_dir()]
    source = next((entry for entry in candidates if find_model_files(entry)), None)
    if source is None:
        raise DownloadError(
            "error: the verified archive did not contain model.int8.onnx and tokens.txt"
        )

    target.mkdir(parents=True, exist_ok=True)
    for entry in source.iterdir():
        if entry.name != TOKENS_FILE_NAME and entry.name not in MODEL_FILE_NAMES:
            continue
        destination = target / entry.name
        temporary = target / (entry.name + ".partial")
        try:
            shutil.copy2(entry, temporary)
            os.replace(temporary, destination)
        finally:
            if temporary.exists():
                temporary.unlink()


def download_model(target: Path) -> dict[str, object]:
    """Explicitly download, verify, and install the pinned INT8 model."""
    if find_model_files(target) is not None:
        return {
            "status": "already_present",
            "model_dir": str(target),
            "sha256": DOWNLOAD_SHA256,
        }

    target.parent.mkdir(parents=True, exist_ok=True)
    work_dir = tempfile.mkdtemp(prefix="vibdock-stt-download-")
    archive = Path(work_dir) / "sensevoice-int8.tar.bz2"
    extracted = Path(work_dir) / "extracted"
    try:
        sys.stderr.write(f"downloading pinned model archive from {DOWNLOAD_URL}\n")
        _download_archive(archive)
        sys.stderr.write("sha256 verified; extracting\n")
        extracted.mkdir()
        _extract_archive(archive, extracted)
        _install_from_extracted(extracted, target)
    finally:
        shutil.rmtree(work_dir, ignore_errors=True)

    if find_model_files(target) is None:
        raise DownloadError("error: model install completed but the files are unusable")

    return {
        "status": "downloaded",
        "model_dir": str(target),
        "source": DOWNLOAD_URL,
        "size": DOWNLOAD_SIZE,
        "sha256": DOWNLOAD_SHA256,
    }


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Dev-only prototype: transcribe one WAV with a local SenseVoice "
            "model. Audio stays on this machine."
        ),
        epilog=(
            "Exit codes: 0 success, 1 audio/inference/download failure, "
            "2 invalid arguments, 3 missing dependency, 4 no usable model."
        ),
    )
    parser.add_argument(
        "wav",
        nargs="?",
        help="PCM WAV file to transcribe; optional only with --download",
    )
    parser.add_argument(
        "--model-dir",
        help=(
            "explicit model directory; overrides discovery and fails closed if "
            "it is not a usable model"
        ),
    )
    parser.add_argument(
        "--download",
        action="store_true",
        help="explicitly download the pinned INT8 model (requires --consent-download)",
    )
    parser.add_argument(
        "--consent-download",
        action="store_true",
        help=(
            "confirm that you reviewed the pinned source, size, SHA-256 and "
            "model license before this explicit download"
        ),
    )
    parser.add_argument(
        "--threads",
        type=int,
        default=2,
        help="inference threads (default: 2)",
    )
    parser.add_argument(
        "--language",
        default="auto",
        choices=["auto", "zh", "en", "ja", "ko", "yue"],
        help="SenseVoice language hint (default: auto)",
    )
    parser.add_argument(
        "--no-itn",
        action="store_true",
        help="disable inverse text normalization (numbers, punctuation)",
    )
    args = parser.parse_args(argv)
    if args.threads < 1:
        parser.error("--threads must be at least 1")
    if args.consent_download and not args.download:
        parser.error("--consent-download is only meaningful with --download")
    if not args.wav and not args.download:
        parser.error("a WAV path is required unless --download is used")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)

    installed_dir: Path | None = None
    if args.download:
        if not args.consent_download:
            sys.stderr.write(
                "error: --download requires --consent-download.\n"
                "\n"
                "This is an explicit download, not an automatic one. Review:\n"
                f"  source:  {DOWNLOAD_URL}\n"
                f"  size:    {DOWNLOAD_SIZE:,} bytes\n"
                f"  sha256:  {DOWNLOAD_SHA256}\n"
                f"  license: {LICENSE_URL}\n"
                f"  model:   {MODEL_REFERENCE_URL}\n"
            )
            return 2

        target = (
            Path(args.model_dir).expanduser()
            if args.model_dir
            else vibdock_model_dir()
        )
        try:
            info = download_model(target)
        except (DownloadError, OSError, urllib.error.URLError) as exc:
            sys.stderr.write(f"{exc}\n")
            return 1
        installed_dir = target
        sys.stderr.write(
            f"model ready at {info['model_dir']}; upstream SenseVoice declares "
            f"the MIT license ({LICENSE_URL})\n"
        )
        if not args.wav:
            print(json.dumps(info, ensure_ascii=False))
            return 0

    wav_path = Path(args.wav).expanduser()
    if not wav_path.is_file():
        sys.stderr.write(f"error: WAV file not found: {wav_path}\n")
        return 2

    try:
        model_dir = installed_dir or resolve_model_dir(args.model_dir)
    except ModelNotFound as exc:
        sys.stderr.write(str(exc))
        return 4

    try:
        result = transcribe(
            wav_path,
            model_dir,
            threads=args.threads,
            language=args.language,
            use_itn=not args.no_itn,
        )
    except ModelNotFound as exc:
        sys.stderr.write(str(exc) + "\n")
        return 4
    except RuntimeError as exc:
        sys.stderr.write(str(exc) + "\n")
        return 1

    print(json.dumps(result, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
