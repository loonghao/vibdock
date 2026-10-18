// Real Windows service + real browser coverage for the application-shortcut UI.
// The shortcut is a saved-state fixture: no application is launched, no hotkey
// is pressed and no audio action is executed.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const net = require('node:net');
const {spawn} = require('node:child_process');
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const binary = path.resolve(process.argv[2] || 'target/debug/vibdock.exe');
const output = path.resolve(process.argv[3] || 'work/web-ui-windows');
const chrome = process.env.CHROMIUM_PATH || 'C:/Program Files/Google/Chrome/Application/chrome.exe';
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const entryId = 'a'.repeat(64);
const trigger = 'Ctrl + Alt + Shift + F23';
const dispatch = 'Ctrl + Shift + F23';
async function unusedPort() {
  const server = net.createServer(); await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port; await new Promise(resolve => server.close(resolve)); return port;
}
(async () => {
  fs.mkdirSync(output, {recursive:true});
  for (const name of ['apps-shortcuts-1280.png','apps-shortcuts-390.png']) fs.rmSync(path.join(output,name),{force:true});
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'vibdock-win-web-'));
  const port = await unusedPort(); const origin = `http://127.0.0.1:${port}`;
  const env = {...process.env, LOCALAPPDATA:root, HOME:root, VIBDOCK_PORT:String(port), VIBDOCK_OVERLAY:'0'};
  fs.mkdirSync(path.join(root,'VibDock'));
  fs.writeFileSync(path.join(root,'VibDock','shortcuts.json'), JSON.stringify({
    schema_version:1, revision:2,
    entries:[{id:entryId,label:'Windows Fixture Shortcut',executable_path:'C:\\VibDockFixture\\NeverExecuted.exe',trigger,dispatch,scope:'foreground'}]
  }));
  let child = spawn(binary, ['service-start'], {env, stdio:'ignore'});
  let browser;
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
      assert((await page.locator('#pinnedShortcuts').innerText()).includes(trigger),'saved trigger is missing');
      assert((await page.locator('#pinnedShortcuts').innerText()).includes(dispatch),'saved dispatch is missing');
      assert((await page.locator('#pinnedShortcuts').innerText()).includes('作用域：仅当该应用位于前台'),'saved scope is missing');
      assert((await page.locator('#apps h2').allInnerTexts()).some(text=>text.includes('应用快捷键')),'application shortcut section is missing');
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true,'software view overflows the viewport');
      assert.deepEqual(errors,[],'Windows software page raised a JavaScript error');
      await page.screenshot({path:path.join(output,`apps-shortcuts-${viewport.width}.png`),fullPage:true});
      await context.close();
    }
    const stop = await fetch(`${origin}/api/shutdown`,{method:'POST',headers:{Authorization:`Bearer ${record.token}`}});
    assert(stop.ok,'isolated Windows service did not accept shutdown');
    fs.writeFileSync(path.join(output,'report.json'),JSON.stringify({
      sourceRevision:process.env.VIBDOCK_SOURCE_REVISION||process.env.GITHUB_SHA||null,
      browser:await browser.version(),service:'real isolated Windows process',
      savedShortcutFixture:true,dispatchAvailable:true,pathRedaction:true,views:[1280,390],
      exclusions:['no hotkey press','no application launch','no audio mutation']
    },null,2));
    console.log(`PASS: real Windows service and ${await browser.version()} rendered the application-shortcut section at 1280/390px with saved trigger, dispatch, scope, redaction and no JavaScript errors.`);
  } finally {
    if (browser) await browser.close();
    const stopped = spawn(binary, ['service-stop'], {env, stdio:'ignore'});
    await Promise.race([new Promise(resolve=>stopped.on('exit',resolve)),delay(5000)]);
    if (stopped.exitCode===null) stopped.kill();
    await Promise.race([new Promise(resolve=>child.exitCode===null?child.on('exit',resolve):resolve()),delay(3000)]);
    if (child.exitCode===null) child.kill();
    fs.rmSync(root,{recursive:true,force:true});
  }
})().catch(error=>{console.error(String(error.stack||error));process.exitCode=1});
