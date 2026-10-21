import base64
import hashlib
import hmac
import http.client
import io
import json
from pathlib import Path
import ssl
import tempfile
import threading
import time
import unittest
import wave
from unittest.mock import patch
import bridge


class Tests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / 'service.json'
        self.path.write_text(json.dumps({'port': 12345, 'token': 'fixture-not-a-secret'}))
        self.bridge = bridge.Bridge(self.path, ['saved'], True)
        self.actions = [{'id': 'saved', 'name': 'Saved', 'steps': [{'type':'send_keys','keys':'Alt + V'}]}]
        self.count = 0
        def local(method, path, body=None):
            if path == '/api/config': return {'device_id':'mic-exact'}
            if path == '/api/actions': return {'bindings':self.actions}
            if path in ('/api/pad/execute', '/api/actions/toggle'):
                self.count += 1
                return {'state':'DONE','muted':True}
            if path == '/api/actions/status': return {'muted':False,'device_id':body['device_id']}
            raise AssertionError(path)
        self.bridge.local = local
        self.bridge.approve()
        self.session = self.bridge.pair(self.bridge.ticket)['session']
    def tearDown(self):
        self.temp.cleanup()
    def request(self, **extra):
        return {'target':self.bridge.target,'sequence':1,'kind':'action','id':'saved',**extra}
    def test_pairing_is_one_use_expiring_and_separate_from_service_token(self):
        with self.assertRaises(ValueError): self.bridge.pair('invalid')
        self.assertNotIn('fixture',self.session)
        with self.assertRaises(ValueError): self.bridge.session('invalid')
        self.bridge.sessions[self.session]['until'] = 0
        with self.assertRaises(ValueError): self.bridge.session(self.session)
        other = bridge.Bridge(self.path, [])
        ticket=other.ticket;other.ticket_until=0
        with self.assertRaises(ValueError): other.pair(ticket)
    def test_concurrent_duplicate_toggle_runs_once(self):
        results=[]
        threads=[threading.Thread(target=lambda:results.append(self.bridge.execute(self.session,self.request(kind='microphone')))) for _ in range(8)]
        for t in threads:t.start()
        for t in threads:t.join()
        self.assertEqual(self.count,1)
        self.assertEqual(results,[results[0]]*8)
        with self.assertRaises(ValueError):self.bridge.execute(self.session,self.request(kind='action'))
    def test_ambiguous_failure_is_consumed_and_never_retried(self):
        def fail(*_args): self.count+=1;raise OSError('dispatch result unknown')
        self.bridge.local=fail
        first=self.bridge.execute(self.session,self.request())
        self.assertFalse(first['ok'])
        self.assertEqual(self.bridge.execute(self.session,self.request()),first)
        self.assertEqual(self.count,1)
    def test_target_sequence_allowlist_and_digest(self):
        with self.assertRaises(ValueError):self.bridge.execute(self.session,self.request(target='remote PC'))
        with self.assertRaises(ValueError):self.bridge.execute(self.session,self.request(sequence=2))
        self.assertFalse(self.bridge.execute(self.session,self.request(id='unknown'))['ok'])
        self.assertEqual(self.count,0)
        self.actions[0]['steps']=[{'type':'send_keys','keys':'Alt + X'}]
        self.assertFalse(self.bridge.execute(self.session,self.request(sequence=2))['ok'])
        self.assertEqual(self.count,0)
    def test_changed_microphone_is_rejected_without_fallback(self):
        self.bridge.device='removed'
        self.assertFalse(self.bridge.execute(self.session,self.request(kind='microphone'))['ok'])
        self.assertEqual(self.count,0)
    def test_missing_speech_is_explicit_and_cancel_does_not_inject(self):
        answer=self.bridge.execute(self.session,self.request(kind='speech',wav=''))
        self.assertFalse(answer['ok']);self.assertIn('not configured',answer['error'])
        self.assertTrue(self.bridge.cancel(self.session,2)['cancelled'])
        self.assertEqual(self.count,0)
    def test_wav_validation_before_engine_execution(self):
        self.bridge.stt=Path('missing.exe');self.bridge.model=Path('missing-model')
        for audio in [b'not wav',b'x'*(bridge.MAX_AUDIO+1)]:
            with self.assertRaises(Exception):self.bridge.transcribe(self.session,1,base64.b64encode(audio))
        stream=io.BytesIO()
        with wave.open(stream,'wb') as w:w.setnchannels(2);w.setsampwidth(2);w.setframerate(16000);w.writeframes(b'\0'*40)
        with self.assertRaises(ValueError):self.bridge.transcribe(self.session,1,base64.b64encode(stream.getvalue()))
    def test_cancel_before_recognition_never_starts_process(self):
        self.bridge.stt=Path('missing.exe');self.bridge.model=Path('missing-model')
        stream=io.BytesIO()
        with wave.open(stream,'wb') as w:w.setnchannels(1);w.setsampwidth(2);w.setframerate(16000);w.writeframes(b'\0'*40)
        self.bridge.cancel(self.session,1)
        with patch('subprocess.Popen') as launch:
            with self.assertRaises(ValueError):self.bridge.transcribe(self.session,1,base64.b64encode(stream.getvalue()))
            launch.assert_not_called()
    def test_https_origin_auth_size_and_pairing_over_real_socket(self):
        cert,key=bridge.certificates(Path(self.temp.name)/'tls','127.0.0.1')
        server=bridge.Server(('127.0.0.1',0),bridge.handler(self.bridge,'placeholder'))
        host=f'127.0.0.1:{server.server_port}'
        server.RequestHandlerClass=bridge.handler(self.bridge,host)
        context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.load_cert_chain(cert,key)
        original=server.finish_request
        def finish(request,address):
            with context.wrap_socket(request,server_side=True) as secured:original(secured,address)
        server.finish_request=finish
        thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
        try:
            ca=ssl.create_default_context(cadata=ssl.DER_cert_to_PEM_cert((Path(self.temp.name)/'tls/phone-ca.cer').read_bytes()))
            def request(path,body,origin='valid',token=None):
                conn=http.client.HTTPSConnection('127.0.0.1',server.server_port,context=ca,timeout=3)
                headers={'Content-Type':'application/json','Origin':'https://'+host if origin=='valid' else origin}
                if token:headers['Authorization']='Bearer '+token
                conn.request('POST',path,json.dumps(body),headers);r=conn.getresponse();result=(r.status,json.loads(r.read()));conn.close();return result
            self.assertEqual(request('/status',{},origin='https://evil.invalid',token=self.session)[0],403)
            self.assertEqual(request('/status',{})[0],400)
            self.assertEqual(request('/status',{},token=self.session)[0],200)
            self.assertEqual(request('/execute',self.request(),token=self.session)[1]['ok'],True)
            self.assertEqual(request('/execute',self.request(),token=self.session)[1]['ok'],True)
            self.assertEqual(self.count,1)
            self.assertEqual(request('/pair',{'ticket':'expired'})[0],400)
            with patch.object(bridge, 'REQUEST_DEADLINE', 0.25):
                slow=http.client.HTTPSConnection('127.0.0.1',server.server_port,context=ca,timeout=2)
                slow.connect()
                slow.sock.sendall(b'POST /status HTTP/1.1\r\nHost: ')
                started=time.monotonic()
                self.assertEqual(slow.sock.recv(1),b'')
                self.assertLess(time.monotonic()-started,2)
                slow.close()
        finally:
            server.shutdown();server.server_close();thread.join()


if __name__=='__main__':unittest.main()
