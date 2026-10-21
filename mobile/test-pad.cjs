// Pure lifecycle tests: no browser, audio device, injected input or UI automation.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const elements=new Map();
function element(id){if(!elements.has(id))elements.set(id,{disabled:false,dataset:{},classList:{add(){},remove(){}},setPointerCapture(){},textContent:'',replaceChildren(){},append(){}});return elements.get(id);}
const document={getElementById:element,querySelectorAll:()=>[],addEventListener(){}};
let resolvePermission,stopped=0,uploads=0;
const context=vm.createContext({document,navigator:{mediaDevices:{getUserMedia:()=>new Promise(resolve=>resolvePermission=resolve)}},isSecureContext:true,window:{addEventListener(){}},fetch:async()=>{uploads++;throw Error('No network expected');},setTimeout:()=>1,clearTimeout(){},Float32Array,ArrayBuffer,DataView,Uint8Array,btoa:s=>Buffer.from(s,'binary').toString('base64')});
const script=fs.readFileSync('mobile/pad.js','utf8');vm.runInContext(script.slice(0,script.lastIndexOf('(async()=>')),context);
const run=code=>vm.runInContext(code,context);
(async()=>{
 run("online=true;speech=true;controls();");
 const pending=run("startRecording({pointerId:1,preventDefault(){}})");
 assert.equal(run('busy'),true);
 await run('stopRecording(true)');
 resolvePermission({getTracks:()=>[{stop(){stopped++;}}]});await pending;
 assert.equal(stopped,1,'late microphone permission must stop its tracks');
 assert.equal(run('recording'),null);assert.equal(uploads,0);
 run("online=true;speech=true;controls();");
 const cancelled=run("startRecording({pointerId:2,preventDefault(){}})");
 await run("$('voice').onpointercancel()");
 resolvePermission({getTracks:()=>[{stop(){stopped++;}}]});await cancelled;
 assert.equal(stopped,2);assert.equal(uploads,0);
 const clip=run('wav(new Float32Array(16000*31),16000)');
 const buffer=Buffer.from(clip,'base64');assert.equal(buffer.readUInt32LE(40),480000*2,'recording must be capped at exactly 30 seconds');
 run('busy=true;online=true;recording={};controls()');
 assert.equal(element('voice').disabled,false,'recording control must retain pointer capture');
 console.log('PASS: late permission, pointer cancel, track cleanup and WAV deadline (pure JS)');
})().catch(error=>{console.error(error);process.exitCode=1;});
