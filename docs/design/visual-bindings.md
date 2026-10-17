# Visual audio and action bindings

The user controls one local workbench through either application cards and
binding editors or an agent skill. Both surfaces use the same authenticated,
same-origin, typed commands and readback. Existing device IDs, software pins,
and the original single microphone shortcut remain compatible.

## Requested behavior and acceptance

| Surface | Required behavior | Evidence |
| --- | --- | --- |
| Taskbar feedback | A microphone status cell remains visible without software pins. Software logos keep playback state separate. | Exact native endpoint identity, freshness and failure tests; native pointer/DPI acceptance separately. |
| Microphone diagnostics | Select an exact endpoint, see mute and level separately, explicitly run a bounded signal test. | Real native capture/readback; no automatic unmute, no samples stored or transmitted. Silence is not a broken-device diagnosis. |
| Application cards | Discover readable running software, show supported actions, open its binding detail. | Native process/session identities; unsupported and unavailable remain explicit. |
| Independent audio controls | Bind input and output actions separately, including one key to silence selected agent playback during a meeting. | Exact endpoint/session readback, with meeting playback and unrelated endpoints preserved. |
| Meeting assistance | Detect supported meeting-app presence; prompt for a configured mute action instead of muting automatically. | Process presence, microphone activity and verified meeting state are distinct signals. |
| Visual bindings | Record or select Windows keys, save multiple trigger/action bindings, retain old registrations on conflicts/save failure. | Whole-set native registration and rollback tests, including the legacy shortcut. |
| Key sequences | One trigger dispatches bounded combinations and delays; cancellation/focus change releases owned keys. | Typed validation, bounded worker, target/focus fences, recursion rejection and cleanup tests. Delivery is not application-state verification. |
| Agent skill | Discover capabilities, inspect and save bindings, request explicit supported actions, read results. | Token never printed; same verified-instance protocol and command contracts as the UI. |
| Future devices | Add a typed action adapter without changing trigger storage or leaking platform code into the command contract. | Closed action kinds and reported capabilities; no arbitrary shell execution. |

## Scope of microphone control

Device mute affects every application using the endpoint. Software playback
mute addresses an explicitly selected current session. An association alone
does not change an application's input route or establish voice activity.

Windows privacy consent is a different layer: ordinary desktop applications
cannot be individually toggled on the microphone privacy page. The UI may
open the relevant Windows settings, but must not claim a device mute or an
internal audio-policy route is a per-application privacy grant. Microsoft
documents this distinction in [camera, microphone and privacy](https://support.microsoft.com/en-us/windows/privacy/windows-camera-microphone-and-privacy).

## Interaction direction

The main screen is a compact control desk for developers moving between an
agent and a meeting. Application cards lead to a detail editor with independent
input and playback rows, a short signal meter, and an action sequence timeline.
Keep the existing original logo and teal accent. Use neutral state glyphs for
unmuted endpoints; a recording dot would imply an unverified voice session.

The domain is signal paths, endpoints, channels, switches and timed actions.
Use the existing graphite/white surfaces, soft gray controls, teal active
selection, amber unavailable and red mute/error. The signature is a visible
trigger-to-actions sequence with the affected endpoint/application next to
every step. Avoid generic metric dashboards, invented device imagery and
success states based only on saved drafts.

## Execution boundaries

Global triggers, native input and audio adapters remain separate from the
validated command/configuration contract. Native work must stay off the HTTP
and UI owner threads. Key sequences are finite; the first implementation is a
triggered sequence with per-step delays, not an unattended recurring scheduler.
OS-reserved keys and conflicts return their actual failure rather than becoming
apparently armed bindings. All new mutation/status routes preserve browser
session scope, Host/Origin validation and explicit target identity.

Source, CI, candidate installation and real-client acceptance are separate
gates. WeChat Input, Codex Voice, Dot, Mini, Tencent Meeting and WeCom scenarios
require their own actual client state transitions; offline fixtures do not
certify them.

## Current source contracts

- `GET /api/bindings/capabilities` reports the platform, canonical key choices,
  action kinds and limits. `GET/POST /api/bindings` uses a revision and the whole
  typed binding set. `POST /api/bindings/cancel` requests cooperative cancellation.
- `POST /api/audio/tests` accepts only an exact `device_id` and returns an
  operation reference. Reads and cancellation use that reference and the same
  authenticated principal. One pending test is allowed; unconfirmed native
  cleanup prevents further tests until service restart. Test frames are drained
  and discarded, while only bounded aggregate measurements leave the helper.
- `/api/apps` returns the native application catalog, independent microphone
  state and guarded routing capability/device lists. Presence pins have no
  invented playback endpoint. Browser-supplied paths or process IDs never
  authorize an action.
- `POST /api/apps/routes` accepts the current revision, a fresh selection token,
  explicit capture/render endpoint and unique Windows roles. Its principal-scoped
  receipt distinguishes verified policy, verified rollback and unconfirmed
  outcomes. Preferred policy never establishes a stream or voice transition.
- `vibdock control capabilities|config|bindings|apps|cancel` uses the native
  verified-instance client. Config and binding writes accept bounded JSON;
  packaged Skills use this contract without reading credentials.

The Windows preferred-policy adapter is build-gated and capability-probed. It
uses a guarded internal interface whose availability can change with Windows;
an unavailable probe disables controls. It never changes microphone privacy
consent. Shared capture-session volume cannot provide independent application
microphone mute: Microsoft documents that it is coupled to endpoint volume in
[IAudioEndpointVolume](https://learn.microsoft.com/en-us/windows/win32/api/endpointvolume/nn-endpointvolume-iaudioendpointvolume).

## Remaining integration acceptance

Meeting assistance must distinguish a Tencent Meeting/WeCom process from a
verified meeting. Any suggestion requires a human-selected explicit mute action;
it must not automatically unmute or restore stale state. The global binding
editor already permits independent input/playback actions, but automatic meeting
recognition and resident prompts are not implemented by these source contracts.

Codex permission cards require a human to inspect and choose allow or deny.
A future explicit integration may offer that human a shortcut to the visible
request. An agent Skill cannot approve its own tools, synthesize approval input
or create an automatic allow-all policy. No permission-card control is currently
implemented.

Recurring scheduling and non-audio device adapters remain future work; finite
per-step delays are the implemented timing model. End-to-end fixtures cover
saved bindings for six named scenarios, auth, revision conflicts and restart
behavior. They do not prove installed software voice behavior, meeting detection
or isolation of concurrent real application streams.
