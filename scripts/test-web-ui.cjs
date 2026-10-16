// Real browser + real isolated service. No mocked routes or audio devices.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const net = require('node:net');
const {spawn} = require('node:child_process');
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const binary = path.resolve(process.argv[2] || 'target/debug/vibdock');
const output = path.resolve(process.argv[3] || 'work/web-ui');
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function unusedPort() {
  const server = net.createServer(); await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port; await new Promise(resolve => server.close(resolve)); return port;
}
(async () => {
  fs.mkdirSync(output, {recursive:true});
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'vibdock-web-'));
  const port = await unusedPort(); const origin = `http://127.0.0.1:${port}`;
  const env = {...process.env, LOCALAPPDATA:root, HOME:root, VIBDOCK_PORT:String(port), VIBDOCK_OVERLAY:'0'};
  const child = spawn(binary, ['service-start'], {env, stdio:'ignore'});
  let browser;
  try {
    let ready = false;
    for(let i=0;i<100;i++){ try{ const response=await fetch(`${origin}/health`); if(response.ok){ready=true;break} }catch{} await delay(100); }
    assert(ready,'real service did not start');
    browser = await chromium.launch({headless:true, ...(process.env.CHROMIUM_PATH ? {executablePath:process.env.CHROMIUM_PATH}: {})});
    const results=[];
    for(const viewport of [{width:1280,height:900},{width:390,height:844},{width:320,height:740}]) {
      const page = await browser.newPage({viewport,colorScheme:'dark'});
      const external=[]; const errors=[];
      page.on('request', request => { if(!request.url().startsWith(origin) && !request.url().startsWith('data:')) external.push(request.url()); });
      page.on('pageerror', error=>errors.push(error.message));
      await page.goto(origin, {waitUntil:'networkidle'});
      const logo = page.getByRole('img',{name:'VibDock 标志'}); await logo.waitFor();
      assert(await logo.evaluate(img=>img.complete&&img.naturalWidth===512),'original SVG did not render');
      assert.equal((await logo.getAttribute('src')).split(',')[1],fs.readFileSync('assets/brand/vibdock-symbol-light.svg').toString('base64'),'homepage logo differs from supplied SVG');
      assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),'page overflows mobile width');
      const box=await logo.boundingBox();assert(box.width>=32&&box.height>=32);
      assert.equal(await page.evaluate(()=>localStorage.getItem('vibdock.token')),null);
      assert.deepEqual(external,[],'page loaded external resources'); assert.deepEqual(errors,[]);
      await page.screenshot({path:path.join(output,`homepage-${viewport.width}.png`),fullPage:true});
      results.push({viewport,logo:box,externalRequests:0,javascriptErrors:0,mode:await page.locator('#banner').innerText()});
      await page.close();
    }
    // Inspect the actual unmodified light and teal SVGs on matching backgrounds.
    const page=await browser.newPage({viewport:{width:800,height:400}});
    const images=['light','teal'].map(name=>({name,source:fs.readFileSync(`assets/brand/vibdock-symbol${name==='light'?'-light':''}.svg`).toString('base64')}));
    await page.setContent(`<html><body style="margin:0;display:flex">${images.map(({name,source})=>`<section style="background:${name==='light'?'#10233a':'#faf9f6'};width:400px;height:400px;display:grid;place-content:center"><img alt="VibDock ${name}" width="256" height="256" src="data:image/svg+xml;base64,${source}"></section>`).join('')}</body></html>`);
    for(const image of await page.locator('img').all())assert(await image.evaluate(img=>img.complete&&img.naturalWidth===512));
    await page.screenshot({path:path.join(output,'original-logo-light-dark.png')});
    fs.writeFileSync(path.join(output,'report.json'),JSON.stringify({browser:await browser.version(),service:'real isolated process',mockedRoutes:false,results,exclusions:['native Windows launcher','actual audio/hardware','macOS menu bar']},null,2));
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
})().catch(error=>{console.error(error);process.exitCode=1});
