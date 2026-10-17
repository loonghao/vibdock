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
  run(`mode='service';serviceToken='new-session';captureDevices=[{id:'mic-a',name:'Desk mic'}];multiCaps={native_bindings:true,max_bindings:8,max_actions:8,key_catalog:['Enter','MediaPlayPause']};renderMultiState({revision:1,bindings:[{id:'one',label:'Meeting mute',enabled:true,trigger:'Ctrl + F9',actions:[{type:'microphone',device_id:'mic-a',operation:'mute'}]}],activation:[{id:'one',state:'registered'}],busy:false});selectMultiBinding('one');multiDraft.label='Unsaved';multiDirty=true;renderMultiState({...multiData,revision:2});`);
  assert.equal(run('multiDraft.label'), 'Unsaved', 'poll discarded edited binding');
  assert.equal(run('multiDraft.revision'), 1, 'poll silently rebased a stale draft');
  run(`api=async(path,options)=>{receivedRequest={path,body:JSON.parse(options.body)};return {ok:false,status:409,text:async()=>JSON.stringify({error:'Revision changed'})}};`);
  await run('saveMultiBinding()');
  assert.equal(run('receivedRequest.body.revision'), 1);
  assert.equal(run('multiDraft.label'), 'Unsaved', 'conflict erased draft');
  assert.equal(elements.get('multiEditStatus').dataset.kind, 'error');
  run(`api=async()=>({ok:true,status:200,text:async()=>JSON.stringify({...multiData,revision:3,bindings:[{...multiDraft,revision:undefined}]})});`);
  await run('saveMultiBinding()');
  assert.equal(run('multiDraft'), null);
  assert.equal(run('multiData.revision'), 3);
  run(`renderMultiState({revision:2,bindings:[]});`);
  assert.equal(run('multiData.revision'),3,'late old GET replaced newer saved bindings');
  assert.equal(writes, 0, 'new bindings wrote browser persistence');
  run(`renderMultiState({...multiData,execution:{binding_id:'one',state:'verified',completed_actions:1,results:[{device_id:'mic-a',state:'verified',muted:true}]}});`);
  assert.match(elements.get('multiExecution').textContent, /音频动作已回读验证/);
  assert.equal(elements.get('multiExecution').children.length,1,'step receipts were hidden');
  run(`selectMultiBinding('one');multiRecording=true;stopMultiRecording();`);
  assert.equal(run('multiRecording'),false);
  run(`multiSaving=true;let beforeActions=JSON.stringify(multiDraft.actions);$('multiActions').children[0].children[2].children[2].onclick();`);
  assert.equal(run('JSON.stringify(multiDraft.actions)'),run('beforeActions'),'in-flight save allowed action deletion');
  run(`multiSaving=false;`);
  run(`updateProbeDevices();$('probeDevice').value='mic-a';renderMicSummary({device_id:'wrong',state:'unmuted',age_ms:0,level_state:'available',peak:0.5});`);
  assert.equal(elements.get('signalLine').dataset.unknown, 'true', 'meter accepted another microphone');
  run(`renderMicSummary({device_id:'mic-a',state:'muted',age_ms:0,level_state:'available',peak:0.5});`);
  assert.equal(elements.get('micLevel').value, 0.5);
  assert.match(elements.get('micLevelLabel').textContent, /静音前/);
  run(`renderMicSummary({device_id:'mic-a',state:'unmuted',age_ms:5000,level_state:'available',peak:0.5});`);
  assert.equal(elements.get('signalLine').dataset.unknown, 'true', 'stale meter stayed live');
  run(`let testJob={id:'${'b'.repeat(64)}',device:'mic-a',session:serviceToken,started:0};microphoneJob=testJob;finishMicTest({operation_id:testJob.id,device_id:'mic-a',state:'completed',result:{device_id:'wrong',captured_frames:10,sample_count:10,level_state:'available',peak:0.3,muted:false,signal_detected:true}},testJob);`);
  assert.equal(elements.get('micTestResult').dataset.kind, 'error', 'test accepted another device result');
  run(`microphoneJob=testJob;finishMicTest({operation_id:testJob.id,device_id:'mic-a',state:'completed',result:{device_id:'mic-a',captured_frames:10,sample_count:10,level_state:'available',peak:0,muted:true,signal_detected:false}},testJob);`);
  assert.equal(elements.get('micTestResult').dataset.kind, 'ok', 'verified silence was treated as unavailable');
  assert.match(elements.get('micTestResult').textContent, /设备当前已静音/);
  run(`lastAppsData={platform:'windows',revision:4,storage_writable:true,entries:[{id:'${entryId}',label:'Editor',routing_selection_id:'${'c'.repeat(64)}'}],routing_capabilities:{state:'available'},routing_devices:{available:true,capture:[{id:'mic-a',name:'Desk mic'}],render:[{id:'speaker-a',name:'Speakers'}]}};renderAppBinding(lastAppsData);selectAppBinding('${entryId}');updatePreferredRoute();`);
  assert.equal(elements.get('routeDevice').value, '', 'route silently selected first endpoint');
  assert.equal(elements.get('savePreferredRoute').disabled, true);
  run(`$('routeDevice').value='mic-a';updatePreferredRoute();let routeFixture={id:'${'d'.repeat(64)}',process:123,device:'mic-a',flow:'capture',label:'Editor',session:serviceToken};routeJob=routeFixture;finishPreferredRoute({operation_id:routeFixture.id,process_id:999,device_id:'mic-a',flow:'capture',state:'verified',stream_state:'unknown',roles:[0,1,2].map(role=>({role,device_id:'mic-a',verified:true}))},routeFixture);`);
  assert.equal(elements.get('routeResult').dataset.kind, 'error', 'route accepted another process receipt');
  run(`routeJob=routeFixture;finishPreferredRoute({operation_id:routeFixture.id,process_id:123,device_id:'mic-a',flow:'capture',state:'verified',stream_state:'unknown',roles:[0,1,2].map(role=>({role,device_id:'mic-a',verified:true}))},routeFixture);`);
  assert.equal(elements.get('routeResult').dataset.kind, 'ok');
  assert.match(elements.get('routeResult').textContent, /实际音频流仍未确认/);
  run(`api=async()=>({ok:false,status:401,text:async()=>{throw Error('text/plain must not be parsed')}});`);
  await assert.rejects(run(`workbenchRequest('/api/bindings')`), /会话失效/);
  assert.equal(run('serviceToken'), null);
  assert.equal(elements.get('saveMultiBinding').disabled, true);
  assert.equal(elements.get('startMicTest').disabled, true);
  console.log('PASS: authoritative saves, draft conflicts, auth expiry, microphone identity/freshness and test receipts (DOM fixtures only)');
})().catch(error => {console.error(error);process.exitCode=1;});
