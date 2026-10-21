'use strict';
const $=id=>document.getElementById(id);
let session='',target='',sequence=1,busy=false,online=false,speech=false,recording=null,activeSpeech=0;
const say=message=>{$('status').textContent=message;};
async function api(path,data={}){
 const controller=new AbortController(),timer=setTimeout(()=>controller.abort(),path==='/execute'?70000:5000);
 try{
  const response=await fetch(path,{method:'POST',headers:{'Content-Type':'application/json',Authorization:'Bearer '+session},body:JSON.stringify(data),cache:'no-store',redirect:'error',signal:controller.signal});
  const value=await response.json();if(!response.ok)throw Error(value.error||'请求失败');return value;
 }finally{clearTimeout(timer);}
}
function controls(){
 document.querySelectorAll('#actions button').forEach(b=>b.disabled=busy||!online);
 $('mic').disabled=busy||!online||!$('mic').dataset.enabled;
 $('voice').disabled=!recording&&(busy||!online||!speech||!isSecureContext||!navigator.mediaDevices);
 $('cancel').disabled=!recording&&!activeSpeech;
}
async function dispatch(kind,id,extra={}){
 if(busy)return;busy=true;const number=sequence++;if(kind==='speech')activeSpeech=number;controls();
 try{const answer=await api('/execute',{target,sequence:number,kind,id,...extra});
  if(!answer.ok)throw Error(answer.error);
  if(kind==='speech'){
   if(activeSpeech===number){$('text').value=JSON.stringify(answer.result,null,2);say('已返回识别结果。');}
  }else say('目标：'+target+'\n'+JSON.stringify(answer.result));
 }catch(error){say(error.message+'\n不会自动重试；结果不明时请先查看电脑状态。');}
 finally{activeSpeech=0;busy=false;controls();}
}
async function refresh(){
 if(!session)return;
 try{const state=await api('/status');if(state.target!==target)throw Error('目标电脑已改变，请重新配对');
  online=true;speech=state.speech;
  const fingerprint=JSON.stringify(state.actions);if($('actions').dataset.fingerprint!==fingerprint){
   $('actions').replaceChildren();for(const action of state.actions){const b=document.createElement('button');b.textContent=action.name;b.onclick=()=>dispatch('action',action.id);$('actions').append(b);}$('actions').dataset.fingerprint=fingerprint;
  }
  $('mic').dataset.enabled=state.microphone_enabled?'yes':'';
  $('mic').textContent=state.microphone?'麦克风：'+(state.microphone.muted?'已静音':'已开启')+' · 点按切换':'未允许麦克风控制 / 未选择端点';
  $('gap').textContent=state.speech_gap||(!isSecureContext?'录音需要手机信任本地 HTTPS 证书。':'');controls();
 }catch(error){online=false;await stopRecording(true);if(activeSpeech)cancelRecognition();say('连接或状态读取失败：'+error.message);controls();}
}
function wav(samples,rate){
 const length=Math.min(480000,Math.floor(samples.length*16000/rate)),buffer=new ArrayBuffer(44+length*2),view=new DataView(buffer);
 const word=(at,text)=>{for(let i=0;i<text.length;i++)view.setUint8(at+i,text.charCodeAt(i));};
 word(0,'RIFF');view.setUint32(4,36+length*2,true);word(8,'WAVE');word(12,'fmt ');view.setUint32(16,16,true);view.setUint16(20,1,true);view.setUint16(22,1,true);view.setUint32(24,16000,true);view.setUint32(28,32000,true);view.setUint16(32,2,true);view.setUint16(34,16,true);word(36,'data');view.setUint32(40,length*2,true);
 for(let i=0;i<length;i++){const sample=Math.max(-1,Math.min(1,samples[Math.floor(i*rate/16000)]));view.setInt16(44+i*2,sample<0?sample*32768:sample*32767,true);}
 let encoded='';const bytes=new Uint8Array(buffer);for(let i=0;i<bytes.length;i+=8192)encoded+=String.fromCharCode(...bytes.subarray(i,i+8192));return btoa(encoded);
}
async function startRecording(event){
 if($('voice').disabled||recording)return;event.preventDefault();$('voice').setPointerCapture(event.pointerId);
 const record={cancelled:false,chunks:[],count:0,stream:null,context:null};recording=record;busy=true;controls();$('voice').disabled=false;$('voice').classList.add('active');say('正在请求麦克风授权…');
 try{const stream=await navigator.mediaDevices.getUserMedia({audio:{channelCount:1},video:false});record.stream=stream;
  if(record.cancelled||recording!==record){stream.getTracks().forEach(t=>t.stop());return;}
  const context=new AudioContext();record.context=context;await context.resume();
  if(record.cancelled||recording!==record){await context.close();return;}
  const source=context.createMediaStreamSource(stream),processor=context.createScriptProcessor(4096,1,1),silent=context.createGain();silent.gain.value=0;record.processor=processor;
  processor.onaudioprocess=e=>{if(record.cancelled)return;const chunk=e.inputBuffer.getChannelData(0).slice();record.chunks.push(chunk);record.count+=chunk.length;if(record.count/context.sampleRate>=30)stopRecording(false);};
  source.connect(processor);processor.connect(silent);silent.connect(context.destination);record.timer=setTimeout(()=>stopRecording(false),30000);say('正在录音 · 松开转文字');controls();$('voice').disabled=false;
 }catch(error){say('录音不可用：'+error.message);await stopRecording(true);}
}
async function stopRecording(cancelled){
 const record=recording;if(!record)return;recording=null;record.cancelled=true;clearTimeout(record.timer);
 record.stream?.getTracks().forEach(t=>t.stop());if(record.processor){record.processor.onaudioprocess=null;record.processor.disconnect();}
 if(record.context&&record.context.state!=='closed')await record.context.close();$('voice').classList.remove('active');busy=false;controls();
 if(cancelled||!record.count){say('录音已取消，没有发送音频。');return;}
 const samples=new Float32Array(record.count);let at=0;for(const chunk of record.chunks){samples.set(chunk,at);at+=chunk.length;}
 await dispatch('speech',undefined,{wav:wav(samples,record.context.sampleRate)});
}
function cancelRecognition(){const number=activeSpeech;activeSpeech=0;if(number)api('/cancel',{sequence:number}).catch(()=>{});say('已取消；不会自动插入文字。');controls();}
$('voice').onpointerdown=startRecording;$('voice').onpointerup=()=>stopRecording(false);$('voice').onpointercancel=()=>stopRecording(true);$('voice').onlostpointercapture=()=>{if(recording)stopRecording(true);};
$('cancel').onclick=()=>{stopRecording(true);cancelRecognition();};$('mic').onclick=()=>dispatch('microphone');
document.addEventListener('visibilitychange',()=>{if(document.hidden){stopRecording(true);cancelRecognition();}});
window.addEventListener('offline',()=>{online=false;stopRecording(true);cancelRecognition();controls();});
window.addEventListener('pagehide',()=>{stopRecording(true);if(activeSpeech)fetch('/cancel',{method:'POST',headers:{'Content-Type':'application/json',Authorization:'Bearer '+session},body:JSON.stringify({sequence:activeSpeech}),keepalive:true}).catch(()=>{});});
(async()=>{const ticket=location.hash.slice(1);history.replaceState(null,'',location.pathname);try{const paired=await api('/pair',{ticket});session=paired.session;target=paired.target;$('target').textContent='控制目标：'+target;say('配对成功。仅执行笔记本允许的动作。');await refresh();setInterval(refresh,2000);}catch(error){say(error.message);}})();
