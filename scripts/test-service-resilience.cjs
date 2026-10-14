const fs = require("node:fs"),
  path = require("node:path"),
  net = require("node:net"),
  assert = require("node:assert/strict");
const { spawn } = require("node:child_process");
const os = require("node:os");
if (process.platform !== "win32" || !process.argv[2]) {
  console.error(
    "Usage: node scripts/test-service-resilience.cjs <vibdock.exe> (Windows only)",
  );
  process.exit(2);
}
if (typeof fetch !== "function" || typeof AbortSignal.timeout !== "function") {
  console.error("Node.js 18 or newer is required for development checks.");
  process.exit(2);
}
const binary = path.resolve(process.argv[2]),
  root = fs.mkdtempSync(path.join(os.tmpdir(), "vibdock-resilience-"));
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
let child, port, token;
const failures = [];
async function req(route, options = {}) {
  return fetch(`http://127.0.0.1:${port}${route}`, {
    ...options,
    headers: { Authorization: `Bearer ${token}`, ...options.headers },
    signal: AbortSignal.timeout(1000),
  });
}
(async () => {
  const listener = net.createServer();
  await new Promise((r) => listener.listen(0, "127.0.0.1", r));
  port = listener.address().port;
  await new Promise((r) => listener.close(r));
  child = spawn(binary, ["service-start"], {
    env: { ...process.env, LOCALAPPDATA: root, VIBDOCK_PORT: String(port) },
    windowsHide: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  const log = fs.createWriteStream(path.join(root, "service.log"));
  child.stdout.pipe(log);
  child.stderr.pipe(log);
  const readyDeadline = Date.now() + 15000;
  let ready = false;
  while (Date.now() < readyDeadline) {
    assert.ok(
      child.exitCode === null && child.signalCode === null,
      "Owned service exited before readiness",
    );
    try {
      token = JSON.parse(
        fs.readFileSync(path.join(root, "VibDock/service.json"), "utf8"),
      ).token;
      if ((await req("/health")).ok) {
        ready = true;
        break;
      }
    } catch {}
    await pause(30);
  }
  assert.ok(ready, "Service readiness exceeded 15 seconds");
  assert.ok(token);
  assert.equal((await req("/health")).status, 200);
  const idle = net.createConnection({ host: "127.0.0.1", port });
  idle.on("error", () => {});
  try {
    await new Promise((resolve, reject) => {
      idle.once("connect", resolve);
      idle.once("error", reject);
    });
    await pause(80);
    assert.equal((await req("/health")).status, 200);
    assert.equal((await req("/api/config")).status, 200);
    console.log(
      "PASS idle preconnection: health and authenticated configuration remain responsive",
    );
  } catch (error) {
    failures.push(`idle preconnection: ${error.name}: ${error.message}`);
    console.error(`FAIL ${failures.at(-1)}`);
  } finally {
    idle.destroy();
  }
  await pause(80);
  assert.equal(
    (await req("/health")).status,
    200,
    "Service must recover after the owned idle connection closes",
  );

  try {
    const payload = JSON.stringify({ hotkey: "Ctrl + Alt + Shift + F20" });
    const status = await new Promise((resolve, reject) => {
      const socket = net.createConnection({ host: "127.0.0.1", port });
      let text = "";
      socket.setTimeout(2000, () =>
        socket.destroy(new Error("Complete chunked request timed out")),
      );
      socket.on("error", reject);
      socket.on("data", (data) => {
        text += data.toString();
        if (text.length > 65536) {
          socket.destroy(new Error("Response exceeds test bound"));
          return;
        }
        const match = /^HTTP\/1\.[01] (\d{3})/.exec(text);
        if (match) {
          resolve(Number(match[1]));
          socket.destroy();
        }
      });
      socket.once("connect", () =>
        socket.write(
          `POST /api/config HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nAuthorization: Bearer ${token}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n${Buffer.byteLength(payload).toString(16)}\r\n${payload}\r\n0\r\n\r\n`,
        ),
      );
    });
    assert.equal(
      status,
      200,
      "A complete, authorized chunked config request must still work",
    );
    assert.equal(
      (await (await req("/api/config")).json()).hotkey,
      "Ctrl + Alt + Shift + F20",
    );
    console.log(
      "PASS complete authorized chunked configuration: persisted without an audio device",
    );
  } catch (error) {
    failures.push(
      `complete authorized chunked: ${error.name}: ${error.message}`,
    );
    console.error(`FAIL ${failures.at(-1)}`);
  }

  for (const item of [
    {
      name: "unauthorized content-length",
      path: "/api/config",
      chunked: false,
      auth: false,
    },
    {
      name: "authorized content-length",
      path: "/api/config",
      chunked: false,
      auth: true,
    },
    {
      name: "unauthorized chunked",
      path: "/api/config",
      chunked: true,
      auth: false,
    },
    {
      name: "bootstrap content-length",
      path: "/api/bootstrap",
      chunked: false,
      auth: false,
    },
    {
      name: "GET with incomplete body",
      method: "GET",
      path: "/health",
      chunked: false,
      auth: false,
    },
  ]) {
    const socket = net.createConnection({ host: "127.0.0.1", port });
    let finished = false;
    socket.on("error", () => {
      finished = true;
    });
    socket.on("close", () => {
      finished = true;
    });
    socket.on("data", () => {
      finished = true;
    });
    try {
      await new Promise((r, j) => {
        socket.once("connect", r);
        socket.once("error", j);
      });
      const framing = item.chunked
        ? "Transfer-Encoding: chunked"
        : "Content-Length: 4096";
      socket.write(
        `${item.method || "POST"} ${item.path} HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\n${framing}\r\n${item.auth ? "Authorization: Bearer " + token + "\r\n" : ""}Connection: close\r\n\r\n${item.chunked ? "1000\r\n{" : "{"}`,
      );
      await pause(80);
      const began = performance.now();
      assert.equal((await req("/health")).status, 200);
      const latency = Math.round(performance.now() - began);
      assert.equal(
        (await req("/api/config")).status,
        200,
        "Authenticated configuration must remain responsive too",
      );
      for (let i = 0; i < 60 && !finished; i++) await pause(100);
      assert.ok(
        finished,
        "Slow request was neither rejected nor closed within 6 seconds",
      );
      console.log(
        `PASS ${item.name}: health ${latency}ms; slow request bounded`,
      );
    } catch (error) {
      failures.push(`${item.name}: ${error.name}: ${error.message}`);
      console.error(`FAIL ${failures.at(-1)}`);
    } finally {
      socket.destroy();
    }
    await pause(80);
    assert.equal(
      (await req("/health")).status,
      200,
      "Owned service must recover after test socket closes",
    );
  }
  const unread = net.createConnection({ host: "127.0.0.1", port });
  unread.on("error", () => {});
  try {
    await new Promise((resolve, reject) => {
      unread.once("connect", resolve);
      unread.once("error", reject);
    });
    unread.pause();
    const get = `GET / HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nConnection: keep-alive\r\n\r\n`;
    unread.write(get.repeat(128));
    await pause(300);
    assert.equal((await req("/health")).status, 200);
    assert.equal(
      (await req("/api/config")).status,
      200,
      "Response backpressure must not block authenticated configuration",
    );
    console.log(
      "PASS nonreading pipelined client: unrelated health and configuration respond",
    );
  } catch (error) {
    failures.push(
      `nonreading pipelined client: ${error.name}: ${error.message}`,
    );
    console.error(`FAIL ${failures.at(-1)}`);
  } finally {
    unread.destroy();
  }
  await pause(100);
  assert.equal(
    (await req("/health")).status,
    200,
    "Service must recover after owned nonreader closes",
  );
  const pending = net.createConnection({ host: "127.0.0.1", port });
  pending.on("error", () => {});
  let shutdownAccepted = false;
  try {
    await new Promise((resolve, reject) => {
      pending.once("connect", resolve);
      pending.once("error", reject);
    });
    pending.write(
      `POST /api/config HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nAuthorization: Bearer ${token}\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n{`,
    );
    await pause(80);
    assert.equal((await req("/api/shutdown", { method: "POST" })).status, 200);
    shutdownAccepted = true;
    for (let i = 0; i < 200 && child.exitCode === null; i++) await pause(30);
    assert.equal(
      child.exitCode,
      0,
      "Shutdown with an unfinished connection must finish within 6 seconds",
    );
    assert.equal(
      fs.existsSync(path.join(root, "VibDock/service.json")),
      false,
      "Shutdown must remove its instance marker",
    );
    console.log(
      "PASS shutdown while an authorized body is unfinished: acknowledged, exit0, marker removed",
    );
  } catch (error) {
    failures.push(
      `shutdown with pending body: ${error.name}: ${error.message}`,
    );
    console.error(`FAIL ${failures.at(-1)}`);
  } finally {
    pending.destroy();
  }
  if (child.exitCode === null) {
    await pause(200);
    try {
      if (child.exitCode === null)
        await req("/api/shutdown", { method: "POST" });
    } catch {}
    for (let i = 0; i < 200 && child.exitCode === null; i++) await pause(30);
  }
  assert.equal(child.exitCode, 0, "Owned service cleanup must finish");
  const rebind = net.createServer();
  await new Promise((resolve, reject) => {
    rebind.once("error", reject);
    rebind.listen(port, "127.0.0.1", resolve);
  });
  await new Promise((resolve) => rebind.close(resolve));
  console.log(
    `Evidence: ${root}; ${failures.length} failed; shutdown acknowledged: ${shutdownAccepted}`,
  );
  if (failures.length) process.exitCode = 1;
})()
  .catch((error) => {
    console.error(error);
    process.exitCode = 1;
  })
  .finally(() => {
    if (child && child.exitCode === null && child.signalCode === null)
      child.kill();
  });
