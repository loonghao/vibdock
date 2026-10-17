// Real isolated HTTP service; saved-software fixtures are not installed-client voice evidence.
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const {spawn} = require('node:child_process');

const usage = 'Usage: node scripts/test-voice-e2e.cjs <vibdock.exe> [--device <exact-capture-endpoint-id>] [--output <evidence-directory>] (Windows, Node 18+)';
const args = process.argv.slice(2);
const binary = args.shift();
let device, output;
while (args.length) {
  const option = args.shift(), value = args.shift();
  if (!value || !['--device', '--output'].includes(option) || (option === '--device' ? device !== undefined : output !== undefined)) {
    console.error(usage); process.exit(2);
  }
  if (option === '--device') device = value; else output = value;
}
if (!binary || process.platform !== 'win32' || typeof fetch !== 'function' || typeof AbortSignal.timeout !== 'function' ||
    (device !== undefined && (!device.trim() || Buffer.byteLength(device) > 512 || /[\x00-\x1f\x7f-\x9f]/.test(device)))) {
  console.error(usage); process.exit(2);
}

const executable = path.resolve(binary);
const runId = `${new Date().toISOString().replace(/[:.]/g, '-')}-${crypto.randomUUID()}`;
output = path.resolve(output || path.join('work', 'voice-e2e', runId));
fs.mkdirSync(output, {recursive:true});
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'vibdock-voice-e2e-'));
const store = path.join(root, 'VibDock');
const appsFile = path.join(store, 'apps.json');
const configFile = path.join(store, 'config.json');
const fixtures = [
  ['wechat_input', '微信输入法'], ['codex_voice', 'Codex voice'],
  ['codex_dot', 'Codex Dot'], ['codex_mini', 'Codex Mini'],
  ['tencent_meeting', 'Tencent Meeting'], ['wecom_meeting', 'WeCom meeting'],
].map(([target, label], index) => ({
  target, id:String(index + 1).repeat(64), label:`${label} — offline saved-software fixture`,
  endpoint_id:'voice-e2e-fixture-output', executable_path:`C:\\VibDockVoiceFixture\\${target}.exe`,
  device_id:`voice-e2e-fixture-microphone-${target}`,
}));
fs.mkdirSync(store);
fs.writeFileSync(appsFile, JSON.stringify({schema_version:1, revision:1, entries:fixtures.map(({target, ...entry}) => entry)}));
fs.writeFileSync(configFile, JSON.stringify({device_id:'voice-e2e-global-fixture-microphone', hotkey:null}));
const globalConfig = fs.readFileSync(configFile);
const secrets = [], processes = [], checks = [];
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
async function waitWithin(promise, ms) {
  let timer;
  try { await Promise.race([promise, new Promise(resolve => { timer = setTimeout(resolve, ms); })]); }
  finally { clearTimeout(timer); }
}
const redact = value => secrets.reduce((text, secret) => text.split(secret).join('[redacted]'), String(value));
const bindings = view => view.entries.map(({id, device_id}) => ({id, device_id}));
let port, origin, env, running, administrator, session, failure;

async function request(route, expected, {method='GET', token=session, body, requestOrigin=origin} = {}) {
  const response = await fetch(`${origin}${route}`, {
    method, headers:{...(token ? {Authorization:`Bearer ${token}`} : {}), ...(requestOrigin ? {Origin:requestOrigin} : {}), ...(body === undefined ? {} : {'Content-Type':'application/json'})},
    ...(body === undefined ? {} : {body:JSON.stringify(body)}), signal:AbortSignal.timeout(5000),
  });
  const text = await response.text();
  assert.equal(response.status, expected, `${route}: expected HTTP ${expected}, received ${response.status}${route.startsWith('/api/bootstrap') ? '' : `; ${text}`}`);
  try { return JSON.parse(text); } catch { return text; }
}

async function bootstrap() {
  const handoff = await request('/api/bootstrap/new', 200, {method:'POST', token:administrator});
  if (typeof handoff.code === 'string') secrets.push(handoff.code);
  assert.match(handoff.code, /^[0-9a-f]{64}$/);
  const redeemed = await request('/api/bootstrap', 200, {method:'POST', token:null, body:{code:handoff.code}});
  if (typeof redeemed.token === 'string') secrets.push(redeemed.token);
  assert.match(redeemed.token, /^[0-9a-f]{64}$/);
  session = redeemed.token;
  await request('/api/bootstrap', 401, {method:'POST', token:null, body:{code:handoff.code}});
  await request('/api/bootstrap/new', 401, {method:'POST'});
}

async function start() {
  const record = {generation:processes.length + 1, stdout:'', stderr:'', exitCode:null, signal:null, shutdownRequested:false};
  const child = spawn(executable, ['service-start'], {env, windowsHide:true, stdio:['ignore','pipe','pipe']});
  record.child = child; processes.push(record); running = record;
  child.stdout.on('data', chunk => { record.stdout += chunk; });
  child.stderr.on('data', chunk => { record.stderr += chunk; });
  child.on('error', error => { record.spawnError = {code:error.code, message:error.message}; });
  record.closed = new Promise(resolve => child.on('close', (code, signal) => {
    record.exitCode = code; record.signal = signal; record.finished = true; resolve();
  }));
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    assert(!record.finished && !record.spawnError, `Service generation ${record.generation} exited before readiness`);
    try {
      const marker = JSON.parse(fs.readFileSync(path.join(store, 'service.json'), 'utf8'));
      assert.equal(marker.port, port);
      administrator = marker.token;
      if (!secrets.includes(administrator)) secrets.push(administrator);
      if ((await fetch(`${origin}/health`, {signal:AbortSignal.timeout(500)})).ok) {
        session = null; return;
      }
    } catch {}
    await pause(40);
  }
  throw new Error(`Service generation ${record.generation} readiness exceeded 15 seconds`);
}

function verifyStopped(record) {
  assert.equal(record.shutdownRequested, true, `Service generation ${record.generation} exited without an explicit shutdown request`);
  assert.equal(record.finished, true, `Service generation ${record.generation} did not finish shutdown`);
  assert.equal(record.exitCode, 0, `Service generation ${record.generation} failed during shutdown`);
  assert.equal(record.signal, null, `Service generation ${record.generation} ended by signal`);
}

async function stop() {
  if (!running) return;
  if (!running.finished) {
    running.shutdownRequested = true;
    await request('/api/shutdown', 200, {method:'POST', token:administrator});
    await waitWithin(running.closed, 8000);
  }
  verifyStopped(running);
  assert(!fs.existsSync(path.join(store, 'service.json')), 'Shutdown retained its credential-bearing instance marker');
}

async function unchangedAfter(route, status, options) {
  const bytes = fs.readFileSync(appsFile), before = await request('/api/apps', 200);
  await request(route, status, options);
  assert(fs.readFileSync(appsFile).equals(bytes), `${route}: rejected request changed saved bytes`);
  const after = await request('/api/apps', 200);
  assert.equal(after.revision, before.revision);
  assert.deepEqual(bindings(after), bindings(before));
}

(async () => {
  const stopped = {generation:0, shutdownRequested:true, finished:true, exitCode:0, signal:null};
  verifyStopped(stopped);
  for (const invalid of [{shutdownRequested:false}, {finished:false}, {exitCode:17}, {signal:'SIGTERM'}]) {
    assert.throws(() => verifyStopped({...stopped, ...invalid}), assert.AssertionError);
  }
  checks.push('harness self-check rejects unexpected, unfinished, nonzero and signalled service exits');
  const listener = net.createServer();
  await new Promise((resolve, reject) => { listener.once('error', reject); listener.listen(0, '127.0.0.1', resolve); });
  port = listener.address().port;
  await new Promise(resolve => listener.close(resolve));
  origin = `http://127.0.0.1:${port}`;
  env = {...process.env, LOCALAPPDATA:root, HOME:root, VIBDOCK_PORT:String(port), VIBDOCK_OVERLAY:'0'};
  await start();
  await request('/api/apps', 401, {token:null});
  await request('/api/apps/bind', 401, {method:'POST', token:null, body:{}});
  for(const route of ['/api/bindings','/api/bindings/capabilities','/api/audio/tests/'+ '0'.repeat(64)])await request(route,401,{token:null});
  await request('/api/audio/tests',401,{method:'POST',token:null,body:{device_id:'not-captured'}});
  await request('/api/apps/routes',401,{method:'POST',token:null,body:{}});
  await request('/api/startup',401,{token:null});
  await request('/api/startup',401,{method:'POST',token:null,body:{enabled:true}});
  await request('/api/bootstrap/new', 401, {method:'POST', token:null});
  await request('/api/bootstrap', 401, {method:'POST', token:null, body:{code:'0'.repeat(64)}});
  await bootstrap();
  await request('/api/apps', 403, {requestOrigin:'https://attacker.invalid'});
  await unchangedAfter('/api/apps/bind', 403, {method:'POST', body:{}, requestOrigin:'https://attacker.invalid'});
  checks.push('one-use bootstrap, browser session scope, unauthenticated and foreign-origin rejection');

  const startup = await request('/api/startup',200);
  assert.equal(typeof startup.supported,'boolean');
  assert(startup.enabled===null||typeof startup.enabled==='boolean');
  assert.equal(startup.effective_state,'unknown','registration must not certify an actual Windows login');
  await request('/api/startup',403,{method:'POST',body:{enabled:true},requestOrigin:'https://attacker.invalid'});
  for(const body of [{},{enabled:'true'},{enabled:false,command:'unused'}])await request('/api/startup',400,{method:'POST',body});
  assert.deepEqual(await request('/api/startup',200),startup,'rejected startup requests changed its registration state');
  checks.push('startup read contract, auth/origin/type/closed-field boundaries; no startup registration changed');

  let view = await request('/api/apps', 200);
  assert.equal(view.platform, 'windows');
  assert.equal(view.entries.length, fixtures.length);
  for (const fixture of fixtures) {
    const entry = view.entries.find(entry => entry.id === fixture.id);
    assert.equal(entry.label, fixture.label);
    assert.equal(entry.device_id, fixture.device_id);
    assert.equal(entry.state, 'unknown');
    assert.deepEqual(entry.sessions, []);
    assert.equal(entry.actions_enabled, false);
    for (const capability of ['start_voice', 'end_voice', 'input_routing']) assert.equal(entry.capabilities[capability], 'not_implemented');
  }
  assert(!JSON.stringify(view).includes('executable_path'));
  checks.push('six explicitly labelled saved-software fixtures; no invented live application, meeting or voice evidence');

  const capabilities=await request('/api/bindings/capabilities',200);
  assert.equal(capabilities.native_bindings,true);
  assert(capabilities.key_catalog.includes('MediaPlayPause'));
  await request('/api/bindings',403,{method:'POST',body:{revision:0,bindings:[]},requestOrigin:'https://attacker.invalid'});
  let actionBindings=await request('/api/bindings',200);
  const scenarioBindings=fixtures.map((fixture,index)=>({id:fixture.target,label:fixture.label,enabled:false,trigger:`Ctrl + Alt + F${index+1}`,actions:[{type:'microphone',device_id:fixture.device_id,operation:'mute'},{type:'playback',entry_id:fixture.id,operation:'mute'},{type:'keys',chord:'Ctrl + Shift + V',delay_ms:100}]}));
  actionBindings=await request('/api/bindings',200,{method:'POST',body:{revision:actionBindings.revision,bindings:scenarioBindings}});
  assert.deepEqual(actionBindings.bindings,scenarioBindings);
  assert(actionBindings.activation.every(binding=>binding.state==='disabled'));
  const actionStore=fs.readFileSync(path.join(store,'bindings.json'));
  await request('/api/bindings',409,{method:'POST',body:{revision:actionBindings.revision-1,bindings:[]}});
  await request('/api/bindings',400,{method:'POST',body:{revision:actionBindings.revision,bindings:[{...scenarioBindings[0],actions:[{type:'shell',command:'unused'}]}]}});
  assert(fs.readFileSync(path.join(store,'bindings.json')).equals(actionStore));
  await request('/api/bindings/cancel',200,{method:'POST',body:{}});
  await request('/api/audio/tests',400,{method:'POST',body:{device_id:''}});
  await request('/api/audio/tests',400,{method:'POST',body:{device_id:'never-opened',unmute:true}});
  await request('/api/audio/tests/'+ '0'.repeat(64),404);
  await request('/api/apps/routes',400,{method:'POST',body:{revision:view.revision,selection_id:'0'.repeat(64),flow:'capture',device_id:'never-routed',roles:[3]}});
  checks.push('six disabled scenario action sequences persist; stale/foreign/arbitrary-action requests rejected; tests and routes validate before native work; no keyboard/audio action executed');

  for (const fixture of fixtures) {
    view = await request('/api/apps', 200);
    const body = {revision:view.revision, id:fixture.id, device_id:fixture.device_id};
    await unchangedAfter('/api/apps/bind', 409, {method:'POST', body:{...body, revision:view.revision - 1}});
    await unchangedAfter('/api/apps/bind', 400, {method:'POST', body:{revision:view.revision, id:fixture.id}});
    await unchangedAfter('/api/apps/bind', 400, {method:'POST', body:{...body, device_id:123}});
    await unchangedAfter('/api/apps/bind', 400, {method:'POST', body:{...body, executable_path:'C:\\Other.exe'}});
    await unchangedAfter('/api/apps/bind', 409, {method:'POST', body:{...body, device_id:`voice-e2e-disconnected-${crypto.randomUUID()}`}});
    checks.push(`${fixture.target}: stale, absent, malformed, forged and disconnected microphone requests preserve bytes/revision/bindings`);
    if (device !== undefined) {
      view = await request('/api/apps/bind', 200, {method:'POST', body:{...body, device_id:device}});
      assert.equal(view.entries.find(entry => entry.id === fixture.id).device_id, device);
      assert.equal(view.revision, body.revision + 1);
      checks.push(`${fixture.target}: explicit microphone association validated by read-only native endpoint status`);
    }
    for (const action of ['start_voice', 'end_voice', 'input_routing']) {
      await unchangedAfter('/api/apps/actions/prepare', 400, {method:'POST', body:{entry_id:fixture.id, session_reference:'0'.repeat(64), action}});
    }
  }
  for (const action of ['start_voice', 'end_voice', 'input_routing']) {
    for (const prefix of ['/api/apps/', '/api/apps/actions/', '/api/actions/']) {
      await unchangedAfter(`${prefix}${action}`, 404, {method:'POST', token:administrator, body:{}});
    }
  }
  checks.push('start_voice, end_voice and input_routing remain not_implemented with no enabled action route');

  const saved = fs.readFileSync(appsFile), expectedBindings = bindings(await request('/api/apps', 200));
  const oldSession = session;
  await stop(); await start();
  await request('/api/apps', 401, {token:oldSession});
  await request('/api/bindings',401,{token:oldSession});
  await bootstrap();
  assert(fs.readFileSync(appsFile).equals(saved), 'Restart replaced persisted software entries');
  view = await request('/api/apps', 200);
  assert.deepEqual(bindings(view), expectedBindings);
  assert.deepEqual((await request('/api/bindings',200)).bindings,scenarioBindings);
  assert(fs.readFileSync(path.join(store,'bindings.json')).equals(actionStore));
  checks.push('all six saved microphone associations and disabled action sequences survive real service restart; previous browser session expires');

  const first = fixtures[0], others = expectedBindings.filter(entry => entry.id !== first.id);
  view = await request('/api/apps/bind', 200, {method:'POST', body:{revision:view.revision, id:first.id, device_id:null}});
  assert.equal(view.entries.find(entry => entry.id === first.id).device_id, null);
  assert.deepEqual(bindings(view).filter(entry => entry.id !== first.id), others);
  const cleared = fs.readFileSync(appsFile);
  assert.equal(JSON.parse(cleared).entries.find(entry => entry.id === first.id).device_id, null);
  view = await request('/api/apps/remove', 200, {method:'POST', body:{revision:view.revision, id:first.id}});
  assert.deepEqual(bindings(view), others);
  assert(fs.readFileSync(configFile).equals(globalConfig), 'App changes altered global microphone/hotkey configuration');
  await stop(); await start(); await bootstrap();
  assert.deepEqual(bindings(await request('/api/apps', 200)), others);
  assert(fs.readFileSync(configFile).equals(globalConfig));
  checks.push('explicit clear then remove affects only WeChat saved fixture; other five and global configuration survive another restart');
  await stop();
})().catch(error => { failure = error; process.exitCode = 1; }).finally(async () => {
  try { if (fs.existsSync(path.join(store, 'service.json'))) secrets.push(JSON.parse(fs.readFileSync(path.join(store, 'service.json'), 'utf8')).token); } catch {}
  if (running && !running.finished) {
    try { await stop(); } catch (error) { if (!failure) { failure = error; process.exitCode = 1; } }
    if (!running.finished) {
      running.child.kill();
      await waitWithin(running.closed, 3000);
    }
  }
  try {
    assert(processes.length > 0, 'No real service generation was started');
    for (const record of processes) verifyStopped(record);
  } catch (error) { if (!failure) { failure = error; process.exitCode = 1; } }
  const processEvidence = processes.map(({generation, child, stdout, stderr, exitCode, signal, spawnError, finished, shutdownRequested}) => {
    fs.writeFileSync(path.join(output, `service-${generation}.stdout.log`), redact(stdout));
    fs.writeFileSync(path.join(output, `service-${generation}.stderr.log`), redact(stderr));
    return {generation, pid:child.pid || null, exitCode, signal, finished:!!finished, shutdownRequested, ...(spawnError ? {spawnError} : {}), stdout:`service-${generation}.stdout.log`, stderr:`service-${generation}.stderr.log`};
  });
  try {
    if (!processes.some(record => !record.finished)) {
      assert.equal(path.dirname(path.resolve(root)), path.resolve(os.tmpdir()));
      assert(path.basename(root).startsWith('vibdock-voice-e2e-'));
      fs.rmSync(root, {recursive:true, force:true});
    }
  } catch (error) { if (!failure) { failure = error; process.exitCode = 1; } }
  const report = {
    status:failure ? 'failed' : 'passed', runId, sourceRevision:process.env.VIBDOCK_SOURCE_REVISION || process.env.GITHUB_SHA || null,
    binarySHA256:fs.existsSync(executable) ? crypto.createHash('sha256').update(fs.readFileSync(executable)).digest('hex') : null,
    service:'real isolated Windows HTTP process', fixtures:fixtures.map(({target, label}) => ({target, label})), checks,
    microphoneAssociation:device === undefined ? {status:'skipped', reason:'No --device supplied; persisted device IDs are metadata fixtures only'} : {status:failure ? 'incomplete' : 'passed', deviceIdSHA256:crypto.createHash('sha256').update(device).digest('hex'), validation:'explicit read-only native endpoint status'},
    audioMutations:0, automaticMicrophoneSelection:false, installedClientAcceptance:{status:'not_checked', targets:fixtures.map(fixture => fixture.target), reason:'Saved software fixtures do not establish real application, voice, Dot or Mini UI behavior'},
    processes:processEvidence, ...(failure ? {error:redact(failure.stack || failure)} : {}),
  };
  fs.writeFileSync(path.join(output, 'report.json'), redact(JSON.stringify(report, null, 2)));
  if (failure) console.error(redact(failure.stack || failure));
  console.log(`${failure ? 'FAIL' : 'PASS'}: six labelled software fixtures, real service auth/persistence/rejection contracts; audio mutations 0; installed-client acceptance not checked. Evidence: ${output}`);
});
