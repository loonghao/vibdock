# Windows service resilience regression

This developer check needs Windows and Node.js 18 or newer. Node.js is not required to run the shipped Rust executables. The script uses only Node.js built-in modules.

```powershell
cargo build --locked --release --bins --target-dir target
node scripts/test-service-resilience.cjs .\target\release\vibdock.exe
```

The check starts its own service with a temporary state directory and dynamically selected loopback port. It does not reuse or stop a service on the normal port. It saves an inactive shortcut without selecting an audio device; it never injects input or changes microphone state.

The nine cases cover:

1. An idle preconnection cannot block health or authenticated configuration.
2. Complete authorized chunked configuration remains supported and persists.
3. An unfinished unauthorized fixed-length POST does not block other requests.
4. An unfinished authorized fixed-length POST does not block other requests.
5. An unfinished unauthorized chunked POST does not block other requests.
6. An unfinished bootstrap POST does not block other requests.
7. A GET carrying an unfinished body remains responsive as a control.
8. A pipelined client that does not read responses cannot block unrelated work.
9. Shutdown is acknowledged and finishes while an authorized body is unfinished; its marker is removed and the port can be rebound.

Both health and authenticated configuration requests must remain responsive. The current budgets are one second for those requests and shutdown acknowledgement, six seconds for slow-request completion or shutdown, and fifteen seconds for initial startup.

Exit 0 means all checks passed; exit 1 means a failed acceptance case; exit 2 means unsupported usage or platform. Logs and the isolated service state are retained under the printed temporary evidence directory. It contains temporary instance credentials: keep the directory private. Cleanup uses only the exact child process and test sockets, not process-name termination or recursive filesystem deletion.

## Current baseline

Before the bounded transport fix, the idle-preconnection, complete-chunked, and GET controls pass, while the other six cases fail with request timeouts. This is a known failing regression, not a green test suite. Keep the test change in draft until the transport candidate passes it independently, then add it to the Windows CI gate. Ordinary Rust unit tests or static platform checks do not replace this boundary check.
