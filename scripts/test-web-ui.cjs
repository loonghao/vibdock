// Real browser + real isolated service. No mocked routes or audio devices.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const net = require('node:net');
const crypto = require('node:crypto');
const {spawn} = require('node:child_process');
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const binary = path.resolve(process.argv[2] || 'target/debug/vibdock');
const output = path.resolve(process.argv[3] || 'work/web-ui');
const redact=[];
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function unusedPort() {
  const server = net.createServer(); await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port; await new Promise(resolve => server.close(resolve)); return port;
}
(async () => {
  fs.mkdirSync(output, {recursive:true});
  for(const name of ['report.json','homepage-1280.png','homepage-390.png','homepage-320.png','original-logo-light-dark.png','apps-1280.png','apps-390.png','apps-320.png'])fs.rmSync(path.join(output,name),{force:true});
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'vibdock-web-'));
  const port = await unusedPort(); const origin = `http://127.0.0.1:${port}`;
  const env = {...process.env, LOCALAPPDATA:root, HOME:root, VIBDOCK_PORT:String(port), VIBDOCK_OVERLAY:'0'};
  // Saved-state fixture only. This is deliberately not an observed audio session.
  fs.mkdirSync(path.join(root,'VibDock'));
  fs.writeFileSync(path.join(root,'VibDock','apps.json'),JSON.stringify({schema_version:1,revision:1,entries:[{id:'a'.repeat(64),label:'离线持久化测试软件 Fixture',endpoint_id:'fixture-output',executable_path:'C:\\VibDockFixture\\NeverExecuted.exe'}]}));
  const child = spawn(binary, ['service-start'], {env, stdio:'ignore'});
  let browser;
  try {
    let ready = false;
    for(let i=0;i<100;i++){ try{ const response=await fetch(`${origin}/health`); if(response.ok){ready=true;break} }catch{} await delay(100); }
    assert(ready,'real service did not start');
    browser = await chromium.launch({headless:true, ...(process.env.CHROMIUM_PATH ? {executablePath:process.env.CHROMIUM_PATH}: {})});
    const record=JSON.parse(fs.readFileSync(path.join(root,'VibDock','service.json')));
    redact.push(record.token);
    async function browserCode(){
      const response=await fetch(`${origin}/api/bootstrap/new`,{method:'POST',headers:{Authorization:`Bearer ${record.token}`}});
      assert(response.ok,'isolated fixture could not create browser handoff');
      const {code}=await response.json();assert(/^[0-9a-f]{64}$/.test(code));redact.push(code);return code;
    }
    const results=[];
    for(const viewport of [{width:1280,height:900},{width:390,height:844},{width:320,height:740}]) {
      const page = await browser.newPage({viewport,colorScheme:'dark'});
      const external=[]; const errors=[];
      page.on('request', request => { if(!request.url().startsWith(origin) && !request.url().startsWith('data:')) external.push(request.url()); });
      page.on('pageerror', error=>errors.push(error.message));
      const code=await browserCode();
      await page.goto(`${origin}/#bootstrap=${code}`, {waitUntil:'networkidle'});
      assert.equal(new URL(page.url()).hash,'','one-use code remained in URL');
      await page.waitForFunction(()=>document.querySelector('#conn').textContent.includes('已授权'));
      assert((await page.locator('#persistNote').innerText()).includes('后台绑定见上方'),'storage labels contradict the live service');
      const replay=await fetch(`${origin}/api/bootstrap`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({code})});
      assert.equal(replay.status,401,'browser code was reusable');
      const storage=await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage}));
      assert.equal(storage,'{}','fresh browser context persisted data, potentially a session credential');
      for(const secret of redact)assert(!storage.includes(secret),'credential persisted in browser storage');
      const logo = page.getByRole('img',{name:'VibDock 标志'}); await logo.waitFor();
      assert(await logo.evaluate(img=>img.complete&&img.naturalWidth===512),'original SVG did not render');
      assert.equal((await logo.getAttribute('src')).split(',')[1],fs.readFileSync('assets/brand/vibdock-symbol-light.svg').toString('base64'),'homepage logo differs from supplied SVG');
      assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),'page overflows mobile width');
      const box=await logo.boundingBox();assert(box.width>=32&&box.height>=32);
      assert.equal(await page.evaluate(()=>localStorage.getItem('vibdock.token')),null);
      assert.deepEqual(external,[],'page loaded external resources'); assert.deepEqual(errors,[]);
      await page.screenshot({path:path.join(output,`homepage-${viewport.width}.png`),fullPage:true});
      await page.locator('button[data-view=apps]').click();
      await page.waitForFunction(()=>!document.querySelector('#appReadStatus').textContent.includes('授权后读取'));
      const apps=await page.evaluate(async()=>{const response=await api('/api/apps');return response.json()});
      assert.equal(apps.actions_enabled,false,'read-only software view enabled an action');
      assert.equal(apps.available,false,'Linux fixture invented native playback evidence');
      assert.equal(apps.entries.length,1);assert.equal(apps.entries[0].state,'unknown');
      assert((await page.locator('#pinnedApps').innerText()).includes('离线持久化测试软件 Fixture'));
      assert(!JSON.stringify(apps).includes('executable_path'),'software view leaked executable locations');
      assert((await page.locator('#appReadStatus').innerText()).length>0);
      assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),'software view overflows mobile width');
      await page.screenshot({path:path.join(output,`apps-${viewport.width}.png`),fullPage:true});
      results.push({viewport,logo:box,externalRequests:0,javascriptErrors:0,mode:await page.locator('#banner').innerText()});
      await page.close();
    }
    assert.equal((await fetch(`${origin}/api/apps`)).status,401,'software metadata exposed without authentication');
    assert.equal((await fetch(`${origin}/api/apps`,{headers:{Authorization:`Bearer ${record.token}`,Origin:'https://attacker.invalid'}})).status,403,'software metadata accepted a foreign origin');
    const settings=await browser.newPage();
    await settings.goto(`${origin}/#bootstrap=${await browserCode()}`,{waitUntil:'networkidle'});
    await settings.waitForFunction(()=>document.querySelector('#conn').textContent.includes('已授权'));
    await settings.locator('button[data-view=apps]').click();
    await settings.getByRole('button',{name:'移除固定'}).click();
    await settings.waitForFunction(()=>document.querySelector('#pinnedApps').textContent.includes('尚未固定软件'));
    assert.equal(JSON.parse(fs.readFileSync(path.join(root,'VibDock','apps.json'))).entries.length,0,'remove did not persist');
    const saved=await settings.evaluate(()=>saveToService({device_id:null,hotkey:'Ctrl + Alt + F9'}));
    assert.equal(saved,true,'idle shortcut configuration was not saved');
    await settings.close();
    const reopened=await browser.newPage();
    await reopened.goto(`${origin}/#bootstrap=${await browserCode()}`,{waitUntil:'networkidle'});
    await reopened.waitForFunction(()=>document.querySelector('#sbHotkey').textContent==='Ctrl + Alt + F9');
    assert.equal(JSON.parse(fs.readFileSync(path.join(root,'VibDock','config.json'))).hotkey,'Ctrl + Alt + F9');
    await reopened.close();
    // Inspect the actual unmodified light and teal SVGs on matching backgrounds.
    const page=await browser.newPage({viewport:{width:800,height:400}});
    const images=['light','teal'].map(name=>({name,source:fs.readFileSync(`assets/brand/vibdock-symbol${name==='light'?'-light':''}.svg`).toString('base64')}));
    await page.setContent(`<html><body style="margin:0;display:flex">${images.map(({name,source})=>`<section style="background:${name==='light'?'#10233a':'#faf9f6'};width:400px;height:400px;display:grid;place-content:center"><img alt="VibDock ${name}" width="256" height="256" src="data:image/svg+xml;base64,${source}"></section>`).join('')}</body></html>`);
    for(const image of await page.locator('img').all())assert(await image.evaluate(img=>img.complete&&img.naturalWidth===512));
    await page.screenshot({path:path.join(output,'original-logo-light-dark.png')});
    fs.writeFileSync(path.join(output,'report.json'),JSON.stringify({sourceRevision:process.env.VIBDOCK_SOURCE_REVISION||process.env.GITHUB_SHA||null,binarySHA256:crypto.createHash('sha256').update(fs.readFileSync(binary)).digest('hex'),browser:await browser.version(),service:'real isolated process',mockedRoutes:false,oneUseHandoff:true,credentialStorage:false,idleShortcutSavedAndReopened:true,softwareReadOnlyAndUnsupportedHonest:true,savedSoftwareFixtureReadRemovedAndReopened:true,results,exclusions:['native Windows launcher','actual audio/hardware','macOS menu bar']},null,2));
    console.log(`PASS: real service homepage at 1280/390/320px; original logo bytes, alt text, no external requests or JS errors. Browser ${await browser.version()}`);
  } finally {
    if(browser)await browser.close();
    const stopped=spawn(binary,['service-stop'],{env,stdio:'ignore'});
    await Promise.race([new Promise(resolve=>stopped.on('exit',resolve)),delay(5000)]);
    if(stopped.exitCode===null)stopped.kill();
    await Promise.race([new Promise(resolve=>child.exitCode===null?child.on('exit',resolve):resolve()),delay(3000)]);
    if(child.exitCode===null)child.kill();
    fs.rmSync(root,{recursive:true,force:true});
  }
})().catch(error=>{console.error(redact.reduce((text,secret)=>text.split(secret).join('[redacted]'),String(error.stack||error)));process.exitCode=1});
