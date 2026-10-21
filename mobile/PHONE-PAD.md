# VibDock phone Pad preview (Windows laptop)

This is a local execution preview. The **laptop** runs VibDock and the phone
bridge; its hostname and service fingerprint are shown on both screens. A remote
computer's input injection cannot control this laptop. No remote recognizer or
configuration service is connected. Nothing installs a VPN, changes a firewall,
opens a public control port, or installs an audio driver.

## Start in an isolated folder

Download the `vibdock-mobile-windows-candidate` CI artifact and extract its inner
ZIP into a new folder. Compare `Get-FileHash .\vibdock-phone-*.zip -Algorithm SHA256`
with the included `.sha256`. Do not overwrite an installed VibDock directory.

Open PowerShell in the extracted folder:

```powershell
.\start-preview.ps1
```

This uses `preview-data` and port 38942, preserving existing settings. Close an
older hardware-owning instance yourself before using physical controls. Creating
phone actions does not require writing AU05 firmware: in the existing action
editor choose source **手机 / 虚拟 Pad**, add allowed shortcut steps and save.
Multiple virtual actions persist in the existing revisioned `actions.json`;
they do not register hotkeys or require a connected keypad/gamepad.
Configure the exact microphone in laptop Settings before enabling its toggle.
**Do not test microphone toggles while on a call.**

To inspect saved action IDs without exposing credentials, use the included helper:

```powershell
$env:LOCALAPPDATA = Join-Path $PWD 'preview-data'
.\vibdock-agent.ps1 actions
```

Use `ipconfig` to find this laptop's private Wi-Fi IPv4 address. Start the bridge
with an explicit allowlist (the example address and ID must be replaced):

```powershell
.\start-phone.ps1 -Listen 192.168.1.42 -AllowAction 'saved-action-id' -AllowMicrophone
```

For microphone-only testing omit `-AllowAction`. For shortcuts-only omit
`-AllowMicrophone`. Repeat IDs as a PowerShell array: `-AllowAction 'id1','id2'`.
The helper executable includes its Python/TLS/QR runtime; Python installation is
not required. The bridge proves the local service identity before sending its
bearer, on the same connection. It never sends that token to the phone.

The laptop opens a loopback pairing panel with the exact target and a one-use
QR valid for two minutes. Keep this panel private. The phone gets only saved
actions explicitly allowed at startup, pinned to their step digest, and the
explicitly approved microphone. If the service, action or microphone changes,
restart the bridge and pair again. Stop the bridge console with Ctrl+C to revoke
all sessions. Phone sessions last at most eight hours and are never saved.

## Phone HTTPS and pairing

Both devices must be on the same trusted Wi-Fi, without client isolation. The
bridge listens **only** on the explicit private address, port 9443. It does not
change firewall rules. If the phone cannot reach the laptop, report that network
policy is blocking the preview; no public-network fallback is provided.

Recording requires a **trusted HTTPS secure context** and microphone permission.
Ordinary LAN HTTP is insufficient. The bridge makes a short-lived local CA and
IP-specific TLS certificate for this run, under `preview-data/VibDock/phone-preview`.
Download the public CA from the laptop pairing panel and transfer that public
`.cer` file to the phone using your existing trusted file-transfer method. Never
transfer `server.key` or `service.json`.

On iOS/iPadOS, install the certificate profile through Settings, then explicitly
enable its root trust under General > About > Certificate Trust Settings. On
Android, install it through the device's CA-certificate setting; menu names and
whether Chrome trusts user CAs vary by OS/vendor policy. Installing a CA changes
phone trust: inspect the certificate name `VibDock Phone local CA` and remove it
when finished. If the browser still reports certificate errors or
`isSecureContext` is false, recording stays unavailable. Do not assume clicking
through a certificate warning enables microphone access.

Then scan the QR. It contains a one-use pairing ticket in the URL fragment,
which is removed immediately after loading. Confirm that the phone target equals
the laptop panel. The local CA is regenerated on every bridge restart; repeat
phone trust setup for a new run and remove the old CA.

## Use and status

Tap a saved action to execute its existing steps on the displayed laptop. Finite
shortcuts use the existing action executor; no remote key-down or key-up protocol
exists. Responses report service completion/dispatch, not application acceptance.
Microphone state is read back from the selected Windows endpoint and refreshed
every two seconds. Read failure is shown as unknown; there is no default-device
fallback. Requests reserve a sequence number before dispatch; duplicates return
the cached result, failures are consumed, and the bridge never retries an
ambiguous toggle automatically. Requests with another target are rejected.

Press and hold the voice button to record phone audio; release uploads bounded
16 kHz mono PCM WAV and displays transcription. Cancel, pointer cancellation,
page hide, backgrounding or lost connection stop and discard an active recording.
Late microphone permission is also stopped if the pointer was already released.
An in-flight recognition can be cancelled and is limited to 60 seconds. Text is
never injected automatically, and the phone is not a virtual microphone device.

## Speech engine gap and optional existing engine

The standard VibDock candidate lacks the optional `stt` feature and model. The
phone shows this gap and disables recording/transcription; it does not generate
fake text. To use a separately available **VibDock build compiled with `--features
stt`** and an existing SenseVoice model directory:

```powershell
.\start-phone.ps1 -Listen 192.168.1.42 -AllowAction 'saved-id' `
  -SttExecutable 'C:\Tools\VibDock-stt\vibdock.exe' `
  -ModelDirectory 'C:\Models\sensevoice'
```

This invokes the existing bounded `transcribe --wav --model-dir --json` engine,
without shell strings, model download or remote credential use. Unsupported
engines, missing model files and nonzero engine exits are explicit errors. An
existing trusted remote recognition/configuration channel can be integrated in a
follow-up; none is guessed or provisioned by this preview.

## Stop and rollback

Ctrl+C in the phone bridge console immediately revokes pairing and stops its
recognizer subprocess. Exit the isolated VibDock tray (or `vibdock service-stop`
in a PowerShell window with the same `LOCALAPPDATA` and `VIBDOCK_PORT`). Remove the
local CA from the phone. Delete this extracted folder and `preview-data` if no
longer needed. Start your previous installation normally; its files were not
modified. If you explicitly changed AU05 firmware, restore those bindings yourself.

## Evidence and limits

The automated suite checks real TLS sockets, certificate verification, strict
origin checks, session authentication, expiry/replay, target selection, action
digests, simultaneous duplicate toggles, cancellation-before-recognition, malformed
WAVs and absent engines. A real isolated loopback service test checks pinned
ownership proof without injecting keys or changing audio. CI freezes and smoke-
tests the Windows helper and publishes the ZIP/checksum. Native three-platform
Rust CI remains unchanged in scope.

These checks do not qualify a real phone/browser permission flow, Wi-Fi policies,
Windows audio, Ulanzi, IME behavior or an available speech model. Perform those
acceptance steps on the laptop after review. There is no cloud UI-control route.
