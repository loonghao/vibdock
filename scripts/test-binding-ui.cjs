// Dependency-free DOM logic tests. No browser, UI input, or audio device is used.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
class Element {
  constructor(tag = 'div') { this.tag = tag; this.children = []; this.dataset = {}; this.hidden = false; this.disabled = false; this.textContent = ''; this._value = ''; this.classList = {add(){}, remove(){}, toggle(){}}; }
  get options() { return this.children; }
  get value() { return this.tag === 'select' && !this.children.some(o => o.value === this._value) ? '' : this._value; }
  set value(value) { this._value = value; }
  append(...nodes) { this.children.push(...nodes); }
  replaceChildren(...nodes) { this.children = nodes; }
  setAttribute(key, value) { this[key] = value; }
  focus() {}
  scrollIntoView() {}
}
const html = fs.readFileSync('docs/design/index.html', 'utf8');
const elements = new Map([...html.matchAll(/<(\w+)[^>]*\bid="([^"]+)"/g)].map(m => [m[2], new Element(m[1])]));
elements.get('steps').children = [new Element(), new Element(), new Element()];
elements.get('target').children = [{value:'mic'}, {value:'codex'}];
elements.get('target').value = 'mic';
let writes = 0;
const context = vm.createContext({
  document: {getElementById: id => elements.get(id), createElement: tag => new Element(tag), createTextNode: text => ({textContent:text}), querySelectorAll: () => [], addEventListener(){}, body:new Element(), hidden:false},
  localStorage: {getItem:()=>null, setItem:()=>writes++},
  Option: class { constructor(text, value) { this.text = text; this.value = value; this.disabled = false; } },
  location:{hash:'',pathname:'/',search:''}, history:{replaceState(){}}, performance:{now:()=>0},
  setTimeout:()=>1, clearTimeout(){}, setInterval:()=>1, clearInterval(){}, URLSearchParams, AbortController,
  fetch:()=>{throw Error('Unexpected network request');}, confirm:()=>true,
});
const script = html.match(/<script>([\s\S]*)<\/script>/)[1];
vm.runInContext(script.slice(0, script.lastIndexOf('renderBindings();loadEditor();')), context);
const run = code => vm.runInContext(code, context);
const entryId = 'a'.repeat(64);
(async () => {
  assert.equal(run(`softwareStatus({state:'connected',complete:true,sessions:[{muted:true,active:true}]},false)[0]`), 'muted');
  assert.equal(run(`softwareStatus({state:'connected',complete:false,sessions:[{muted:true,active:true}]},false)[0]`), 'unknown', 'partial observation became verified icon state');
  assert.equal(run(`softwareStatus({state:'connected',complete:true,sessions:[]},false)[0]`), 'unknown', 'empty sessions became muted');
  assert.equal(run(`softwareStatus({state:'connected',complete:true,sessions:[{muted:false,active:true}]},false)[0]`), 'playing');
  assert.equal(run(`softwareStatus({state:'running'},true)[2]`), '正在运行，语音状态未知');
  run(`mode='service';serviceToken='test-session';captureDevices=[{id:'mic-a',name:'Desk mic'},{id:'mic-b',name:'Headset'}];`);
  run(`fillDeviceSelect($('device'),'');`);
  assert.equal(elements.get('device').value, '', 'enumeration silently selected a microphone');
  run(`fillDeviceSelect($('device'),'missing-mic');`);
  assert.equal(elements.get('device').value, 'missing-mic', 'disconnected identity was lost');
  assert(elements.get('device').options.find(o => o.value === 'missing-mic').disabled);
  run(`saved.set(selected,{target:'mic',action:'mute',shortcut:'Ctrl + F1'});serviceConfig={device_id:'mic-b',hotkey:'Ctrl + Alt + F9',hotkey_live:true};loadEditor();`);
  assert.equal(elements.get('shortcut').value, 'Ctrl + Alt + F9', 'browser draft replaced authoritative shortcut');
  assert.equal(elements.get('device').value, 'mic-b');
  assert.equal(elements.get('action').options.length, 1, 'saved hotkey action could disagree with backend toggle');
  assert.equal(run('dirty()'), false);
  elements.get('device').value = 'mic-a';
  assert.equal(run('dirty()'), true, 'microphone edit was not tracked');
  run(`api=async()=>({ok:false,status:409,text:async()=>JSON.stringify({message:'Conflict'})});`);
  await run('saveBinding()');
  assert.equal(run('serviceConfig.device_id'), 'mic-b', 'failed shortcut save replaced committed state');
  assert.equal(writes, 0, 'service save wrote browser drafts');
  run(`api=async()=>({ok:true,status:200,text:async()=>JSON.stringify({device_id:'mic-a',hotkey:'Ctrl + Alt + F9',hotkey_live:true})});`);
  await run('saveBinding()');
  assert.equal(run('serviceConfig.device_id'), 'mic-a');
  assert.equal(writes, 0, 'successful service save wrote browser drafts');
  run(`let finishEnumeration;api=()=>new Promise(resolve=>finishEnumeration=resolve);let enumeration=loadDevices();$('device').value='mic-b';finishEnumeration({ok:true,text:async()=>JSON.stringify({devices:captureDevices})});`);
  await run('enumeration');
  assert.equal(elements.get('device').value, 'mic-b', 'late enumeration discarded a newer selection');
  run(`api=async()=>({ok:true,text:async()=>JSON.stringify({devices:[]})});`);
  await run('loadDevices()');
  assert.equal(elements.get('device').value, 'mic-b', 'empty enumeration erased the selected identity');
  assert.equal(elements.get('device').disabled, true);
  run(`captureDevices=[{id:'mic-a',name:'Desk mic'},{id:'mic-b',name:'Headset'}];fillDeviceSelect($('device'),'mic-a');api=async()=>({ok:true,text:async()=>JSON.stringify({device_id:'mic-b',muted:true})});`);
  await run(`$('test').onclick()`);
  assert.equal(elements.get('status').dataset.kind, 'error', 'main microphone check accepted another device readback');

  run(`lastAppsData={platform:'windows',storage_writable:true,revision:2,entries:[{id:'${entryId}',label:'Editor',device_id:'mic-a'}]};appRevision=2;renderAppBinding(lastAppsData);selectAppBinding('${entryId}');`);
  assert.equal(elements.get('appBindingTest').disabled, false);
  elements.get('appBindingDevice').value = 'mic-b';
  elements.get('appBindingDevice').onchange();
  run(`renderAppBinding(lastAppsData);`);
  assert.equal(elements.get('appBindingDevice').value, 'mic-b', 'poll discarded unsaved microphone choice');
  run(`let receivedRequest;api=async(path,options)=>{receivedRequest={path,body:JSON.parse(options.body)};return {ok:true,status:200,text:async()=>JSON.stringify({device_id:'mic-a',muted:true})}};`);
  await run(`appMicrophoneAction('status')`);
  assert.equal(run('receivedRequest.body.device_id'), 'mic-a', 'action used unsaved microphone');
  assert.equal(elements.get('appBindingStatus').dataset.kind, 'ok');
  run(`api=async()=>({ok:true,status:200,text:async()=>JSON.stringify({device_id:'mic-b',muted:true})});`);
  await run(`appMicrophoneAction('mute')`);
  assert.equal(elements.get('appBindingStatus').dataset.kind, 'error', 'wrong device readback reported success');
  run(`api=async()=>({ok:false,status:409,json:async()=>({error:'Entry changed'})});`);
  await run('saveAppBinding(false)');
  assert.equal(run('selectedApp().device_id'), 'mic-a', 'failed association save replaced committed binding');
  assert.equal(elements.get('appBindingStatus').dataset.kind, 'error');
  run(`appsRenderKey='';lastAppsData={...lastAppsData,available:false,complete:false,error:null,choices:[]};api=async(path,options)=>{receivedRequest={path,body:JSON.parse(options.body)};return {ok:true,status:200,json:async()=>({...lastAppsData,revision:3,entries:[{...lastAppsData.entries[0],device_id:'mic-b',state:'unknown',sessions:[]}]})}};`);
  await run('saveAppBinding(false)');
  assert.equal(run('receivedRequest.path'), '/api/apps/bind');
  assert.equal(run('receivedRequest.body.revision'), 2);
  assert.equal(run('selectedApp().device_id'), 'mic-b');
  assert.equal(elements.get('appBindingDevice').value, 'mic-b');
  run(`captureDevices=[{id:'mic-a',name:'Desk mic'}];updateAppBindingControls();`);
  assert.equal(elements.get('appBindingTest').disabled, true, 'disconnected microphone fell back');
  run(`api=async()=>({ok:false,status:401,json:async()=>{throw Error('text/plain response')}});`);
  await run(`editApp('bind',{id:'${entryId}',device_id:null})`);
  assert.equal(run('serviceToken'), null, 'text/plain unauthorized response did not expire session');
  assert.equal(elements.get('appMicMute').disabled, true, 'expired session left mutation enabled');
  assert.equal(elements.get('appBinding').hidden, true);

  run(`mode='service';serviceToken='test-session';lastAppsData={platform:'windows',storage_writable:true,revision:4,available:true,applications:[{selection_id:'bg-ref',name:'微信输入法',icon_b64:null,integration:'wechat_input',background:true},{selection_id:'win-ref',name:'Codex',icon_b64:null,integration:'codex',background:false}]};shortcutsData={revision:0,entries:[],dispatch_available:true,storage_writable:true,error:null};renderShortcutApplications();`);
  assert.equal(run(`$('shortcutDetected').children.length`), 2, 'detected applications were not rendered');
  assert.match(run(`$('shortcutDetected').children[0].children[1].children[1].textContent`), /后台运行/);
  assert.equal(run(`$('shortcutDetected').children[0].children[3].disabled`), false, 'background application could not open the editor');
  run(`openShortcutEditor(lastAppsData.applications[0])`);
  assert.equal(run(`$('shortcutGlobal').checked`), true, 'background application did not select the running scope');
  assert.equal(run(`$('shortcutGlobal').disabled`), true, 'background application allowed a foreground-only scope');
  elements.get('shortcutTrigger').value = 'Ctrl + Alt + F20';
  elements.get('shortcutDispatch').value = 'Ctrl + Shift + F20';
  run(`let shortcutRequest;api=async(path,options)=>{shortcutRequest={path,body:JSON.parse(options.body)};return {ok:true,status:200,text:async()=>JSON.stringify({revision:1,entries:[{id:'${'c'.repeat(64)}',label:'微信输入法',trigger:'Ctrl + Alt + F20',dispatch:'Ctrl + Shift + F20',scope:'running',armed:true,error:null}],dispatch_available:true,storage_writable:true,error:null})}};document.hidden=true;`);
  await run('saveShortcut()');
  assert.equal(run('shortcutRequest.path'), '/api/shortcuts/add');
  assert.equal(run('shortcutRequest.body.scope'), 'running', 'background binding lost its running scope');
  assert.equal(elements.get('shortcutEditor').hidden, true);
  run(`openShortcutEditor(lastAppsData.applications[1])`);
  assert.equal(run(`$('shortcutGlobal').checked`), false, 'windowed application defaulted to the running scope');
  assert.equal(run(`$('shortcutGlobal').disabled`), false, 'windowed application could not choose the running scope');
  elements.get('shortcutTrigger').value = 'Ctrl + Alt + F21';
  elements.get('shortcutDispatch').value = 'Ctrl + Shift + F21';
  run(`api=async()=>({ok:false,status:409,text:async()=>JSON.stringify({error:'That trigger is already used by the microphone binding'})});`);
  await run('saveShortcut()');
  assert.equal(elements.get('shortcutEditor').hidden, false, 'failed shortcut save closed the editor');
  assert.equal(elements.get('shortcutStatus').dataset.kind, 'error', 'failed shortcut save reported success');
  assert.match(elements.get('shortcutStatus').textContent, /^未保存：/, 'shortcut save error was overwritten by the next render');
  assert.equal(run(`$('pinnedShortcuts').children.length`), 1, 'failed shortcut save changed the saved list');
  run(`api=async(path,options)=>{shortcutRequest={path,body:JSON.parse(options.body)};return {ok:true,status:200,text:async()=>JSON.stringify({revision:2,entries:[{id:'${'d'.repeat(64)}',label:'Codex',trigger:'Ctrl + Alt + F21',dispatch:'Ctrl + Shift + F21',scope:'foreground',armed:true,error:null}],dispatch_available:true,storage_writable:true,error:null})}};`);
  await run('saveShortcut()');
  assert.equal(run('shortcutRequest.body.scope'), 'foreground', 'windowed binding did not default to foreground');
  assert.equal(run(`$('pinnedShortcuts').children.length`), 1, 'saved shortcut was not rendered');
  assert.equal(run(`$('pinnedShortcuts').children[0].children[1].disabled`), false, 'saved shortcut stayed disabled after the save completed');
  console.log('PASS: authoritative binding, device identity, atomic UI saves, app association, application recognition and per-app shortcut scope (DOM fixtures only)');
})().catch(error => {console.error(error);process.exitCode=1;});
