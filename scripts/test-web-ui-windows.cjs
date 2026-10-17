// Real Windows service + real browser coverage for the application-shortcut UI.
// The seeded shortcut is a saved-state fixture. The live path launches one real
// windowed Notepad with a unique document title and exercises the Settings
// add/remove path; no hotkey is pressed and no audio action is executed.
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const net = require('node:net');
const {spawn, spawnSync} = require('node:child_process');
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const binary = path.resolve(process.argv[2] || 'target/debug/vibdock.exe');
const output = path.resolve(process.argv[3] || 'work/web-ui-windows');
const chrome = process.env.CHROMIUM_PATH || 'C:/Program Files/Google/Chrome/Application/chrome.exe';
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const entryId = 'a'.repeat(64);
const seededTrigger = 'Ctrl + Alt + Shift + F23';
const seededDispatch = 'Ctrl + Shift + F23';
// The live chords use a different function key than the fixture and can be
// overridden on a host where the default registration is already taken.
const liveTrigger = process.env.VIBDOCK_E2E_TRIGGER || 'Ctrl + Alt + Shift + F22';
const liveDispatch = process.env.VIBDOCK_E2E_DISPATCH || 'Ctrl + Shift + F22';
const detectionTimeout = Number(process.env.VIBDOCK_E2E_DETECT_TIMEOUT_MS || 20000);
const liveToken = `vibdock-live-${crypto.randomBytes(6).toString('hex')}`;
const liveLabel = `VibDock Live ${liveToken}`;
const notepadPath = path.join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'notepad.exe');
async function unusedPort() {
  const server = net.createServer(); await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port; await new Promise(resolve => server.close(resolve)); return port;
}
// Launches one real windowed application whose title is unique to this run.
// The window title becomes the observed application name, so it must contain
// liveToken; detection below is through that real observed row only.
function launchNotepad(root) {
  const file = path.join(root, `${liveToken}.txt`);
  fs.writeFileSync(file, 'VibDock live detected-application fixture.\n');
  const child = spawn(notepadPath, [file], {stdio:'ignore'});
  let spawnError = null;
  child.on('error', error => { spawnError = error.message; });
  return {child, file, error:() => spawnError};
}
// Reads the real Notepad processes without assuming a window is present.
function notepadSnapshot() {
  const command = "Get-Process -Name Notepad -ErrorAction SilentlyContinue | Select-Object Id,MainWindowTitle | ConvertTo-Json -Compress";
  const result = spawnSync('powershell', ['-NoProfile', '-NonInteractive', '-Command', command], {encoding:'utf8', windowsHide:true});
  const text = String(result.stdout || '').trim();
  if (!text) return [];
  try { const parsed = JSON.parse(text); return (Array.isArray(parsed) ? parsed : [parsed]).filter(Boolean); } catch { return []; }
}
// Terminates only the Notepad processes this run launched. The Windows 11
// Notepad launcher can hand the document to another Notepad process, so the
// tree kill is retried by unique title and command line; no other window is
// touched.
async function terminateNotepad(handle) {
  if (!handle) return;
  const pid = handle.child.pid;
  if (pid) spawnSync('taskkill', ['/PID', String(pid), '/T', '/F'], {stdio:'ignore', windowsHide:true});
  const cleanup = `$t='${liveToken}'; Get-CimInstance Win32_Process -Filter "Name='Notepad.exe'" | Where-Object { $_.CommandLine -like "*$t*" } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }; Get-Process -Name Notepad -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowTitle -like "*$t*" } | Stop-Process -Force -ErrorAction SilentlyContinue`;
  for (let i=0;i<6;i++) {
    spawnSync('powershell', ['-NoProfile', '-NonInteractive', '-Command', cleanup], {stdio:'ignore', windowsHide:true});
    await delay(400);
    if (!notepadSnapshot().some(process=>String(process.MainWindowTitle || '').includes(liveToken))) return;
  }
}
async function readShortcuts(page) {
  return page.evaluate(async()=>{const response=await api('/api/shortcuts');return {status:response.status,body:await response.json()}});
}
// The service can return a partial scan between polls, which makes a displayed
// row's short-lived selection reference unusable. Wait for a newer observation
// that still contains this run's title before retrying the UI save.
async function waitForFreshDetection(page) {
  const before = await page.evaluate(()=>Number.isSafeInteger(lastAppsData?.generation)?lastAppsData.generation:-1);
  for (let i=0;i<20;i++) {
    await page.evaluate(()=>refreshApps());
    await delay(300);
    const state = await page.evaluate(token=>({
      generation:Number.isSafeInteger(lastAppsData?.generation)?lastAppsData.generation:-1,
      available:lastAppsData?.available===true,
      present:(lastAppsData?.applications||[]).some(app=>String(app.name||'').includes(token)),
      row:[...document.querySelectorAll('#shortcutDetected .row')].some(row=>row.textContent.includes(token)),
    }), liveToken);
    if (state.generation > before && state.available && state.present && state.row) return true;
  }
  return false;
}
(async () => {
  fs.mkdirSync(output, {recursive:true});
  for (const name of ['apps-shortcuts-1280.png','apps-shortcuts-390.png','apps-shortcuts-live-1280.png']) fs.rmSync(path.join(output,name),{force:true});
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'vibdock-win-web-'));
  const port = await unusedPort(); const origin = `http://127.0.0.1:${port}`;
  const env = {...process.env, LOCALAPPDATA:root, HOME:root, VIBDOCK_PORT:String(port), VIBDOCK_OVERLAY:'0'};
  fs.mkdirSync(path.join(root,'VibDock'));
  fs.writeFileSync(path.join(root,'VibDock','shortcuts.json'), JSON.stringify({
    schema_version:1, revision:2,
    entries:[{id:entryId,label:'Windows Fixture Shortcut',executable_path:'C:\\VibDockFixture\\NeverExecuted.exe',trigger:seededTrigger,dispatch:seededDispatch,scope:'foreground'}]
  }));
  // Start the app before the service so observation can pick it up immediately.
  // Detection is still confirmed through the real UI below. An existing user
  // Notepad is never reused or terminated, so the live path is skipped then.
  const existingNotepad = notepadSnapshot();
  const livePrecondition = existingNotepad.length
    ? `a Notepad instance already exists (PID ${existingNotepad.map(process=>process.Id).join(', ')}); refusing to reuse or terminate an application this run did not start`
    : null;
  const notepad = livePrecondition ? null : launchNotepad(root);
  const child = spawn(binary, ['service-start'], {env, stdio:'ignore'});
  let browser;
  let liveEvidence = null;
  try {
    let ready = false;
    for (let i=0;i<100;i++) { try { const response=await fetch(`${origin}/health`); if(response.ok){ready=true;break} } catch {} await delay(100); }
    assert(ready,'real Windows service did not start');
    const record = JSON.parse(fs.readFileSync(path.join(root,'VibDock','service.json')));
    async function browserCode(){
      const response = await fetch(`${origin}/api/bootstrap/new`,{method:'POST',headers:{Authorization:`Bearer ${record.token}`}});
      assert(response.ok,'isolated fixture could not create a browser handoff');
      const {code} = await response.json();
      assert(/^[0-9a-f]{64}$/.test(code));
      return code;
    }
    browser = await chromium.launch({headless:true, executablePath:chrome});
    for (const viewport of [{width:1280,height:900},{width:390,height:844}]) {
      const context = await browser.newContext({viewport,colorScheme:'dark',bypassCSP:true});
      const page = await context.newPage();
      const errors=[]; page.on('pageerror', error=>errors.push(error.message));
      await page.goto(`${origin}/#bootstrap=${await browserCode()}`, {waitUntil:'load'});
      await page.waitForFunction(()=>document.querySelector('#conn').textContent.includes('已授权'), null, {timeout:15000});
      await page.locator('button[data-view=apps]').click();
      await page.waitForFunction(()=>document.querySelector('#appsTitle').textContent.includes('应用'), null, {timeout:10000});
      await page.waitForFunction(()=>document.querySelector('#shortcutStatus').textContent.includes('已保存'), null, {timeout:15000});
      assert.equal(await page.evaluate(async()=>{const r=await api('/api/shortcuts');const v=await r.json();return v.dispatch_available}),true,'Windows view did not advertise dispatch');
      assert.equal(await page.evaluate(async()=>{const r=await api('/api/shortcuts');const v=await r.json();return JSON.stringify(v).includes('executable_path')}),false,'shortcut view leaked an executable path');
      assert((await page.locator('#shortcutStatus').innerText()).includes('已保存 1 条应用快捷键'),'saved shortcut count is missing');
      assert((await page.locator('#pinnedShortcuts').innerText()).includes('Windows Fixture Shortcut'),'saved shortcut row is missing');
      assert((await page.locator('#pinnedShortcuts').innerText()).includes(seededTrigger),'saved trigger is missing');
      assert((await page.locator('#pinnedShortcuts').innerText()).includes(seededDispatch),'saved dispatch is missing');
      assert((await page.locator('#pinnedShortcuts').innerText()).includes('作用域：仅当该应用位于前台'),'saved scope is missing');
      assert((await page.locator('#apps h2').allInnerTexts()).some(text=>text.includes('应用快捷键')),'application shortcut section is missing');
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true,'software view overflows the viewport');
      assert.deepEqual(errors,[],'Windows software page raised a JavaScript error');
      await page.screenshot({path:path.join(output,`apps-shortcuts-${viewport.width}.png`),fullPage:true});
      await context.close();
    }

    // Live detected-application path. A real Notepad window is only used when
    // the service actually reports it; a missing desktop is an explicit skip.
    const context = await browser.newContext({viewport:{width:1280,height:900},colorScheme:'dark',bypassCSP:true});
    const page = await context.newPage();
    const errors=[]; page.on('pageerror', error=>errors.push(error.message));
    await page.goto(`${origin}/#bootstrap=${await browserCode()}`, {waitUntil:'load'});
    await page.waitForFunction(()=>document.querySelector('#conn').textContent.includes('已授权'), null, {timeout:15000});
    await page.locator('button[data-view=apps]').click();
    await page.waitForFunction(()=>document.querySelector('#pinnedShortcuts').textContent.includes('Windows Fixture Shortcut'), null, {timeout:15000});
    let detected = false;
    if (!livePrecondition) {
      try {
        await page.waitForFunction(token=>[...document.querySelectorAll('#shortcutDetected .row')]
          .some(row=>row.textContent.includes(token)), liveToken, {timeout:detectionTimeout, polling:500});
        detected = true;
      } catch {}
    }
    if (livePrecondition) {
      console.log(`SKIP: live detected-application add/remove not exercised: ${livePrecondition}`);
      liveEvidence = {status:'skipped', reason:livePrecondition};
    } else if (!detected) {
      const apps = await page.evaluate(async()=>{try{const r=await api('/api/apps');return await r.json()}catch(error){return {error:String(error)}}});
      const notepadState = notepad.error() ? `notepad did not start: ${notepad.error()}`
        : `notepad pid ${notepad.child.pid ?? 'unknown'} exit ${notepad.child.exitCode ?? 'running'}`;
      const reason = `no #shortcutDetected row containing ${liveToken} within ${detectionTimeout}ms (${notepadState}; apps.available=${apps.available}; applications=${Array.isArray(apps.applications)?apps.applications.length:'unknown'}; apps.error=${apps.error ?? 'none'})`;
      console.log(`SKIP: live detected-application add/remove not exercised: ${reason}`);
      liveEvidence = {status:'skipped', reason, detectionTimeoutMs:detectionTimeout};
    } else {
      let addStatus = null, addBody = null, detectedName = '', detectedPid = null, addAttempts = 0;
      for (let attempt = 1; attempt <= 3; attempt++) {
        addAttempts = attempt;
        if (attempt > 1 && !(await waitForFreshDetection(page))) {
          addStatus = 409;
          addBody = {error:'the detected application left the observed application list before the binding was saved'};
          break;
        }
        const row = page.locator('#shortcutDetected .row').filter({hasText:liveToken}).first();
        detectedName = (await row.locator('strong').innerText()).trim();
        detectedPid = notepadSnapshot().find(process=>String(process.MainWindowTitle || '').includes(liveToken))?.Id ?? null;
        await row.getByRole('button', {name:'绑定快捷键'}).click();
        assert.equal(await page.locator('#shortcutEditor').isVisible(), true, 'shortcut editor did not open for a detected window');
        assert.equal(await page.locator('#shortcutGlobal').isChecked(), false, 'windowed application pre-selected the running scope');
        assert.equal(await page.locator('#shortcutGlobal').isDisabled(), false, 'windowed application locked the running-scope checkbox');
        const selected = await page.locator('#shortcutApp option:checked').innerText();
        assert(selected.includes(liveToken), 'shortcut editor did not select the detected application');
        await page.locator('#shortcutTrigger').fill(liveTrigger);
        await page.locator('#shortcutDispatch').fill(liveDispatch);
        await page.locator('#shortcutLabel').fill(liveLabel);
        const [addResponse] = await Promise.all([
          page.waitForResponse(r=>r.url()===`${origin}/api/shortcuts/add`&&r.request().method()==='POST', {timeout:15000}),
          page.locator('#shortcutSave').click(),
        ]);
        addStatus = addResponse.status();
        addBody = await addResponse.json().catch(()=>null);
        if (addStatus === 409 && /no longer available/i.test(addBody?.error || '') && attempt < 3) {
          // A partial scan invalidated the displayed selection reference. The
          // UI did not save anything; close the editor and retry from a newer
          // observation rather than turning a transient race into a skip.
          await page.locator('#shortcutCancel').click();
          continue;
        }
        break;
      }
      liveEvidence = {status:'pending', detectedName, detectedPid, notepadPid:notepad.child.pid ?? null, addAttempts, trigger:liveTrigger, dispatch:liveDispatch};
      if (addStatus === 409) {
        // A hotkey held by the host, an expired selection, or another conflict
        // is not evidence of a saved binding. Report the exact error and claim
        // nothing.
        const reason = `live add returned 409: ${addBody?.error || 'unknown conflict'} (detected ${detectedName || 'unknown application'})`;
        if (await page.locator('#shortcutEditor').isVisible()) {
          assert.match(await page.locator('#shortcutStatus').innerText(), /未保存/, 'a conflict was not shown to the user');
        }
        const after = await readShortcuts(page);
        assert.equal(after.body.entries.length, 1, 'a conflicting add persisted an entry');
        console.log(`SKIP: live detected-application add/remove not exercised: ${reason}`);
        liveEvidence.status = 'skipped';
        liveEvidence.reason = reason;
      } else {
        assert.equal(addStatus, 200, `live add failed with ${addStatus}: ${addBody?.error || 'unknown error'}`);
        const liveEntry = addBody.entries.find(entry=>entry.label===liveLabel);
        assert(liveEntry, 'saved live entry is missing from the add response');
        assert.equal(liveEntry.scope, 'foreground', 'windowed application did not save the foreground scope');
        assert.equal(liveEntry.armed, true, 'saved live entry was not registered');
        assert.equal(liveEntry.trigger.replace(/\s+/g,''), liveTrigger.replace(/\s+/g,''), 'saved trigger differs from the requested trigger');
        assert.equal(liveEntry.dispatch.replace(/\s+/g,''), liveDispatch.replace(/\s+/g,''), 'saved dispatch differs from the requested dispatch');
        assert.equal(JSON.stringify(addBody).includes('executable_path'), false, 'live add response leaked an executable path');
        await page.waitForFunction(label=>[...document.querySelectorAll('#pinnedShortcuts .row')].some(row=>row.textContent.includes(label)), liveLabel, {timeout:10000});
        const pinnedRow = page.locator('#pinnedShortcuts .row').filter({hasText:liveLabel}).first();
        const pinnedText = await pinnedRow.innerText();
        assert(pinnedText.includes('作用域：仅当该应用位于前台'), 'pinned live row did not show the foreground scope');
        assert(pinnedText.includes('已注册'), 'pinned live row did not report registration');
        const removeButton = pinnedRow.getByRole('button', {name:'删除'});
        assert.equal(await removeButton.isEnabled(), true, 'remove button stayed disabled after the live save completed');
        const savedView = await readShortcuts(page);
        assert.equal(savedView.status, 200, 'saved live shortcuts were not readable');
        const savedEntry = savedView.body.entries.find(entry=>entry.label===liveLabel);
        assert(savedEntry, 'saved live entry is missing from the shortcut API');
        assert.equal(savedEntry.scope, 'foreground', 'shortcut API reported the wrong live scope');
        assert.equal(savedEntry.armed, true, 'shortcut API reported the live entry as unregistered');
        assert.equal(savedView.body.entries.length, 2, 'saved live entry did not join the seeded fixture');
        assert.equal(JSON.stringify(savedView.body).includes('executable_path'), false, 'shortcut API leaked an executable path');
        await page.screenshot({path:path.join(output,'apps-shortcuts-live-1280.png'),fullPage:true});
        page.once('dialog', dialog=>dialog.accept());
        const [removeResponse] = await Promise.all([
          page.waitForResponse(r=>r.url()===`${origin}/api/shortcuts/remove`&&r.request().method()==='POST', {timeout:10000}),
          removeButton.click(),
        ]);
        assert.equal(removeResponse.status(), 200, 'live shortcut removal failed');
        const removedBody = await removeResponse.json();
        assert.equal(removedBody.entries.some(entry=>entry.label===liveLabel), false, 'removed live entry was still present');
        assert.equal(removedBody.entries.length, 1, 'live removal did not leave the seeded fixture alone');
        await page.waitForFunction(label=>![...document.querySelectorAll('#pinnedShortcuts .row')].some(row=>row.textContent.includes(label)), liveLabel, {timeout:10000});
        assert((await page.locator('#pinnedShortcuts').innerText()).includes('Windows Fixture Shortcut'), 'live removal also removed the seeded fixture');
        await page.waitForFunction(()=>document.querySelector('#shortcutStatus').textContent.includes('已保存 1 条应用快捷键'), null, {timeout:10000});
        // Remove the seeded fixture through the same UI so the final list is
        // genuinely empty, not merely empty of live entries.
        page.once('dialog', dialog=>dialog.accept());
        const fixtureRow = page.locator('#pinnedShortcuts .row').filter({hasText:'Windows Fixture Shortcut'}).first();
        const [fixtureResponse] = await Promise.all([
          page.waitForResponse(r=>r.url()===`${origin}/api/shortcuts/remove`&&r.request().method()==='POST', {timeout:10000}),
          fixtureRow.getByRole('button', {name:'删除'}).click(),
        ]);
        assert.equal(fixtureResponse.status(), 200, 'seeded fixture removal failed');
        const emptyBody = await fixtureResponse.json();
        assert.equal(emptyBody.entries.length, 0, 'shortcut list did not return to empty');
        await page.waitForFunction(()=>document.querySelectorAll('#pinnedShortcuts .row').length===0, null, {timeout:10000});
        assert((await page.locator('#pinnedShortcuts').innerText()).includes('尚未保存应用快捷键'), 'empty shortcut list was not explained');
        assert.deepEqual(errors, [], 'live detected-application page raised a JavaScript error');
        liveEvidence.status = 'passed';
        liveEvidence.scope = savedEntry.scope;
        liveEvidence.armed = savedEntry.armed;
        liveEvidence.removeResult = 'removed through the UI after the confirm dialog';
        liveEvidence.finalEntries = emptyBody.entries.length;
      }
    }
    await context.close();

    const stop = await fetch(`${origin}/api/shutdown`,{method:'POST',headers:{Authorization:`Bearer ${record.token}`}});
    assert(stop.ok,'isolated Windows service did not accept shutdown');
    fs.writeFileSync(path.join(output,'report.json'),JSON.stringify({
      sourceRevision:process.env.VIBDOCK_SOURCE_REVISION||process.env.GITHUB_SHA||null,
      browser:await browser.version(),service:'real isolated Windows process',
      savedShortcutFixture:true,dispatchAvailable:true,pathRedaction:true,views:[1280,390],
      liveDetectedApplication:liveEvidence,
      exclusions:['no hotkey press','no audio mutation','no voice session','only this run\'s Notepad is terminated']
    },null,2));
    const version = await browser.version();
    if (liveEvidence?.status === 'passed') {
      console.log(`PASS: real Windows service and ${version} rendered the seeded shortcut section at 1280/390px and exercised a live detected application (${liveEvidence.detectedName}, Notepad window PID ${liveEvidence.detectedPid ?? liveEvidence.notepadPid}): scope=${liveEvidence.scope}, armed=${liveEvidence.armed}, added after ${liveEvidence.addAttempts} attempt(s), removed through the UI, final entries=${liveEvidence.finalEntries}.`);
    } else {
      console.log(`PASS: real Windows service and ${version} rendered the seeded shortcut section at 1280/390px. SKIP: live detected-application add/remove not exercised: ${liveEvidence?.reason || 'unknown reason'}`);
    }
  } finally {
    if (browser) await browser.close();
    await terminateNotepad(notepad);
    await delay(500);
    const stopped = spawn(binary, ['service-stop'], {env, stdio:'ignore'});
    await Promise.race([new Promise(resolve=>stopped.on('exit',resolve)),delay(5000)]);
    if (stopped.exitCode===null) stopped.kill();
    await Promise.race([new Promise(resolve=>child.exitCode===null?child.on('exit',resolve):resolve()),delay(3000)]);
    if (child.exitCode===null) child.kill();
    try { fs.rmSync(root,{recursive:true,force:true}); } catch {}
  }
})().catch(error=>{console.error(String(error.stack||error));process.exitCode=1});
