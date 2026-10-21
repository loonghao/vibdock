"""Opt-in HTTPS phone Pad. Never exposes the resident administrator token."""
import argparse
import base64
import hashlib
import hmac
import http.client
import io
import ipaddress
import html
import json
import os
from pathlib import Path
import secrets
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(getattr(sys, '_MEIPASS', Path(__file__).parent))
MAX_AUDIO = 2_000_000
REQUEST_DEADLINE = 10


class Bridge:
    def __init__(self, state, allow, microphone=False, stt=None, model=None):
        self.state_path = Path(state)
        self.instance = json.loads(self.state_path.read_text(encoding='utf-8'))
        self.allowed = frozenset(allow)
        self.microphone = microphone
        self.stt, self.model = stt, model
        self.target = socket.gethostname() + ' / ' + hashlib.sha256(
            self.instance['token'].encode()).hexdigest()[:12]
        self.ticket = secrets.token_urlsafe(32)
        self.ticket_until = time.monotonic() + 120
        self.sessions = {}
        self.lock = threading.RLock()
        self.job_lock = threading.Lock()
        self.process = None
        self.cancelled = set()
        self.digests = {}
        self.device = None

    def local(self, method, path, body=None):
        # Prove ownership before revealing the bearer, on this SAME connection.
        if json.loads(self.state_path.read_text(encoding='utf-8')) != self.instance:
            raise ValueError('Target service changed. Restart the phone bridge and pair again.')
        port, token = self.instance['port'], self.instance['token']
        conn = http.client.HTTPConnection('127.0.0.1', port, timeout=5)
        try:
            nonce = secrets.token_hex(32)
            conn.request('POST', '/instance/challenge', json.dumps({'nonce': nonce}),
                         {'Content-Type': 'application/json', 'Connection': 'keep-alive'})
            response = conn.getresponse()
            proof = json.loads(response.read(8193))
            expected = hmac.new(token.encode(), b'VibDock instance proof v1\0' + nonce.encode(),
                                hashlib.sha256).hexdigest()
            if response.status != 200 or not hmac.compare_digest(proof.get('proof', ''), expected):
                raise ValueError('The local listener failed ownership proof.')
            if conn.sock is None:
                raise ValueError('The proved connection closed; refusing credential handoff.')
            payload = None if body is None else json.dumps(body)
            conn.request(method, path, payload, {'Authorization': 'Bearer ' + token,
                         'Origin': f'http://127.0.0.1:{port}', 'Content-Type': 'application/json'})
            response = conn.getresponse()
            raw = response.read(1_000_001)
            if len(raw) > 1_000_000:
                raise ValueError('Local response too large.')
            result = json.loads(raw)
            if response.status != 200:
                raise ValueError(result.get('error', 'Local action failed.'))
            return result
        finally:
            conn.close()

    def pair(self, ticket):
        with self.lock:
            if time.monotonic() > self.ticket_until or not self.ticket or not hmac.compare_digest(ticket, self.ticket):
                raise ValueError('Pairing expired or already used. Restart the bridge for a new QR.')
            if len(self.sessions) >= 8:
                raise ValueError('Eight phone sessions already paired; restart bridge to revoke them.')
            self.ticket = ''
            session = secrets.token_urlsafe(32)
            self.sessions[session] = {'until': time.monotonic() + 28800, 'next': 1, 'results': {}}
            return {'session': session, 'target': self.target}

    def session(self, token):
        with self.lock:
            entry = self.sessions.get(token)
            if entry is None or time.monotonic() > entry['until']:
                raise ValueError('Pair again on the laptop.')
            return entry

    @staticmethod
    def digest(action):
        return hashlib.sha256(json.dumps(action.get('steps', []), sort_keys=True).encode()).hexdigest()

    def approve(self):
        actions = self.local('GET', '/api/actions').get('bindings', [])
        self.digests = {a['id']: self.digest(a) for a in actions if a['id'] in self.allowed}
        if self.allowed != self.digests.keys():
            raise ValueError('An allowed id is not an existing saved action. Inspect laptop Settings first.')
        if self.microphone:
            self.device = self.local('GET', '/api/config').get('device_id')
            if not self.device:
                raise ValueError('Select the exact microphone in laptop Settings first.')

    def status(self):
        config = self.local('GET', '/api/config')
        actions = self.local('GET', '/api/actions').get('bindings', [])
        device = self.device
        if self.microphone and config.get('device_id') != device:
            raise ValueError('Selected microphone changed; restart the bridge and pair again.')
        mic = self.local('POST', '/api/actions/status', {'device_id': device}) if device else None
        return {'target': self.target, 'actions': [{'id': a['id'], 'name': a['name']}
                for a in actions if a['id'] in self.allowed and self.digests.get(a['id']) == self.digest(a)], 'microphone': mic,
                'microphone_enabled': bool(device), 'speech': bool(self.stt and self.model),
                'speech_gap': None if self.stt and self.model else
                'No configured local STT executable/model. Use a VibDock stt build and existing SenseVoice model.'}

    def execute(self, token, data):
        entry = self.session(token)
        sequence = data.get('sequence')
        if type(sequence) is not int or sequence < 1 or data.get('target') != self.target:
            raise ValueError('Invalid sequence or target computer.')
        # Serialize dispatch and reserve before I/O. Ambiguous failures are cached,
        # never automatically retried (especially microphone toggles).
        with self.job_lock:
            fingerprint = hashlib.sha256(json.dumps(data, sort_keys=True).encode()).hexdigest()
            previous = entry['results'].get(sequence)
            if previous:
                if previous[0] != fingerprint:
                    raise ValueError('Request id reused with different content.')
                return previous[1]
            if sequence != entry['next'] or sequence > 10000:
                raise ValueError('Out-of-order request; pair again if state was lost.')
            entry['next'] += 1
            try:
                kind = data.get('kind')
                if kind == 'action' and data.get('id') in self.allowed:
                    current = self.local('GET', '/api/actions').get('bindings', [])
                    action = next((a for a in current if a['id'] == data['id']), None)
                    if not action or self.digests.get(data['id']) != self.digest(action):
                        raise ValueError('Approved action changed; restart the bridge and approve again.')
                    result = self.local('POST', '/api/pad/execute', {'id': data['id']})
                elif kind == 'microphone' and self.microphone:
                    device = self.local('GET', '/api/config').get('device_id')
                    if not device or device != self.device:
                        raise ValueError('The approved microphone changed; restart the bridge.')
                    result = self.local('POST', '/api/actions/toggle', {'device_id': device})
                elif kind == 'speech':
                    result = self.transcribe(token, sequence, data.get('wav', ''))
                else:
                    raise ValueError('This operation was not allowed on the laptop.')
                answer = {'ok': True, 'target': self.target, 'result': result}
            except Exception as error:
                answer = {'ok': False, 'target': self.target, 'error': str(error)}
            entry['results'][sequence] = (fingerprint, answer)
            # Only the latest 64 can be replayed; older sequence numbers remain rejected.
            for old in sorted(entry['results'])[:-64]:
                del entry['results'][old]
            return answer

    def transcribe(self, token, sequence, encoded):
        if not self.stt or not self.model:
            raise ValueError('Speech engine/model not configured. No recognition was performed.')
        audio = base64.b64decode(encoded, validate=True)
        if len(audio) > MAX_AUDIO:
            raise ValueError('Recording exceeds the 30 second limit.')
        with wave.open(io.BytesIO(audio)) as wav:
            if (wav.getnchannels(), wav.getsampwidth(), wav.getframerate()) != (1, 2, 16000) or wav.getnframes() > 480000:
                raise ValueError('Expected at most 30 seconds of 16 kHz mono PCM16 WAV.')
        key = (token, sequence)
        with tempfile.TemporaryDirectory(prefix='vibdock-phone-') as temp:
            path = Path(temp) / 'voice.wav'
            path.write_bytes(audio)
            with self.lock:
                if key in self.cancelled:
                    raise ValueError('Recording cancelled.')
                self.process = (key, subprocess.Popen([str(self.stt), 'transcribe', '--wav', str(path),
                    '--model-dir', str(self.model), '--json'], stdout=subprocess.PIPE, stderr=subprocess.PIPE))
                process = self.process[1]
            try:
                out, _ = process.communicate(timeout=60)
                with self.lock:
                    if key in self.cancelled:
                        raise ValueError('Recording cancelled.')
                if process.returncode:
                    raise ValueError('Configured VibDock STT failed. Check stt feature and local model on the laptop.')
                return json.loads(out)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()
                with self.lock:
                    self.process = None
                    self.cancelled.discard(key)

    def cancel(self, token, sequence):
        self.session(token)
        if type(sequence) is not int or sequence < 1 or sequence > 10000:
            raise ValueError('Invalid cancellation id.')
        with self.lock:
            if len(self.cancelled) >= 64:
                raise ValueError('Too many cancellations; restart pairing.')
            key = (token, sequence)
            self.cancelled.add(key)
            if self.process and self.process[0] == key:
                self.process[1].kill()
        return {'cancelled': True}


def certificates(directory, address):
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import rsa
    from cryptography.x509.oid import NameOID, ExtendedKeyUsageOID
    import datetime
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    # Fresh per run; trusted root does not grant access without pairing.
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'VibDock Phone local CA')])
    now = datetime.datetime.now(datetime.timezone.utc)
    ca = (x509.CertificateBuilder().subject_name(name).issuer_name(name).public_key(key.public_key())
          .serial_number(x509.random_serial_number()).not_valid_before(now - datetime.timedelta(minutes=5))
          .not_valid_after(now + datetime.timedelta(days=2)).add_extension(x509.BasicConstraints(ca=True, path_length=0), True)
          .add_extension(x509.SubjectKeyIdentifier.from_public_key(key.public_key()), False)
          .add_extension(x509.AuthorityKeyIdentifier.from_issuer_public_key(key.public_key()), False)
          .add_extension(x509.KeyUsage(False, False, False, False, False, True, True, False, False), True)
          .sign(key, hashes.SHA256()))
    leaf_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    leaf = (x509.CertificateBuilder().subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'VibDock Phone')]))
            .issuer_name(name).public_key(leaf_key.public_key()).serial_number(x509.random_serial_number())
            .not_valid_before(now - datetime.timedelta(minutes=5)).not_valid_after(now + datetime.timedelta(days=2))
            .add_extension(x509.SubjectAlternativeName([x509.IPAddress(ipaddress.ip_address(address))]), False)
            .add_extension(x509.BasicConstraints(ca=False, path_length=None), True)
            .add_extension(x509.SubjectKeyIdentifier.from_public_key(leaf_key.public_key()), False)
            .add_extension(x509.AuthorityKeyIdentifier.from_issuer_public_key(key.public_key()), False)
            .add_extension(x509.KeyUsage(True, False, True, False, False, False, False, False, False), True)
            .add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), False).sign(key, hashes.SHA256()))
    for filename, data in [('phone-ca.cer', ca.public_bytes(serialization.Encoding.DER)),
                           ('server.pem', leaf.public_bytes(serialization.Encoding.PEM)),
                           ('server.key', leaf_key.private_bytes(serialization.Encoding.PEM,
                            serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))]:
        path = directory / filename
        path.write_bytes(data)
        os.chmod(path, 0o600)
    return directory / 'server.pem', directory / 'server.key'


class Server(ThreadingHTTPServer):
    daemon_threads = True
    def __init__(self, address, handler):
        super().__init__(address, handler)
        self.slots = threading.BoundedSemaphore(8)
    def handle_error(self, request, client_address):
        if not isinstance(sys.exc_info()[1], (ConnectionError, TimeoutError, ssl.SSLError)):
            print("Phone bridge request failed; no action result is assumed.", file=sys.stderr)

    def process_request(self, request, client_address):
        if not self.slots.acquire(blocking=False):
            request.close()
            return
        super().process_request(request, client_address)
    def process_request_thread(self, request, client_address):
        try:
            request.settimeout(5)
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()


def handler(bridge, host, admin=False, admin_secret='', phone_url='', ca=None):
    class Handler(BaseHTTPRequestHandler):
        def setup(self):
            super().setup()
            def expire():
                try:
                    self.connection.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
            self.deadline = threading.Timer(REQUEST_DEADLINE, expire)
            self.deadline.daemon = True
            self.deadline.start()
        def finish(self):
            self.deadline.cancel()
            super().finish()
        def log_message(self, *_args):
            pass  # Never log pairing URLs, session tokens, audio, or transcripts.
        def reply(self, code, value, mime='application/json'):
            raw = json.dumps(value).encode() if mime == 'application/json' else value
            self.send_response(code)
            self.send_header('Content-Type', mime)
            self.send_header('Content-Length', str(len(raw)))
            self.send_header('Cache-Control', 'no-store')
            self.send_header('Referrer-Policy', 'no-referrer')
            self.send_header('X-Content-Type-Options', 'nosniff')
            self.send_header('Content-Security-Policy', "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")
            self.end_headers()
            self.wfile.write(raw)
        def do_GET(self):
            self.deadline.cancel()
            if self.headers.get('Host') != host:
                return self.reply(403, {'error': 'Invalid Host'})
            if admin:
                if not hmac.compare_digest(self.path, '/' + admin_secret):
                    return self.reply(404, {'error': 'Not found'})
                import qrcode
                import qrcode.image.svg
                # Renew only from the secret laptop panel, retaining this run's certificate.
                with bridge.lock:
                    bridge.ticket = secrets.token_urlsafe(32)
                    bridge.ticket_until = time.monotonic() + 120
                    current_url = phone_url.split('#', 1)[0] + '#' + bridge.ticket
                qr = qrcode.make(current_url, image_factory=qrcode.image.svg.SvgPathImage)
                stream = io.BytesIO()
                qr.save(stream)
                image = base64.b64encode(stream.getvalue()).decode()
                cert = base64.b64encode(ca.read_bytes()).decode()
                html = (f'<meta charset="utf-8"><title>VibDock phone pairing</title><h1>Target: {html.escape(bridge.target)}</h1>'
                        f'<p>Same Wi-Fi. Install and trust the local CA on your phone first (see PHONE-PAD.md). '
                        f'Expires in 2 minutes, usable once. Only selected saved actions are allowed.</p>'
                        f'<a download="vibdock-phone-ca.cer" href="data:application/pkix-cert;base64,{cert}">Download local CA</a>'
                        f'<p><img width="320" src="data:image/svg+xml;base64,{image}"></p>'
                        f'<p>{current_url}</p><p>Refresh this laptop panel to renew the QR after certificate setup.</p><p>Stop this console to revoke all access. No remote PC keyboard is controlled.</p>')
                return self.reply(200, html.encode(), 'text/html; charset=utf-8')
            if self.path in ('/', '/pad.js'):
                file = ROOT / ('pad.html' if self.path == '/' else 'pad.js')
                return self.reply(200, file.read_bytes(), 'text/html; charset=utf-8' if self.path == '/' else 'text/javascript')
            return self.reply(404, {'error': 'Not found'})
        def do_POST(self):
            try:
                lengths = self.headers.get_all('Content-Length', [])
                if len(lengths) != 1 or self.headers.get('Transfer-Encoding'):
                    return self.reply(400, {'error': 'Use one bounded Content-Length'})
                length = int(lengths[0])
                if length < 2 or length > 2_800_000:
                    return self.reply(413, {'error': 'Request too large'})
                raw = self.rfile.read(length)
                if len(raw) != length:
                    raise ValueError('Incomplete request body')
                self.deadline.cancel()
                if admin or self.headers.get_all('Host') != [host] or self.headers.get_all('Origin') != ['https://' + host]:
                    return self.reply(403, {'error': 'Invalid browser origin'})
                if self.headers.get_all('Content-Type') != ['application/json']:
                    return self.reply(400, {'error': 'Use bounded JSON'})
                data = json.loads(raw)
                if not isinstance(data, dict):
                    raise ValueError('Expected an object')
                if self.path == '/pair':
                    return self.reply(200, bridge.pair(str(data.get('ticket', ''))))
                token = self.headers.get('Authorization', '').removeprefix('Bearer ')
                bridge.session(token)
                if self.path == '/status':
                    result = bridge.status()
                elif self.path == '/execute':
                    result = bridge.execute(token, data)
                elif self.path == '/cancel':
                    result = bridge.cancel(token, data.get('sequence'))
                else:
                    return self.reply(404, {'error': 'Not found'})
                self.reply(200, result)
            except Exception as error:
                self.reply(400, {'error': str(error)})
    return Handler


def main():
    if sys.argv[1:] == ['--self-test']:
        import qrcode
        import qrcode.image.svg
        with tempfile.TemporaryDirectory(prefix='vibdock-phone-selftest-') as temp:
            cert, key = certificates(Path(temp), '127.0.0.1')
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(cert, key)
            qrcode.make('https://127.0.0.1/#test', image_factory=qrcode.image.svg.SvgPathImage)
            assert (ROOT / 'pad.html').is_file() and (ROOT / 'pad.js').is_file()
        print('PASS: bundled TLS, certificate, QR and Pad assets')
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--listen', required=True, help='Explicit laptop Wi-Fi IPv4 address (never 0.0.0.0)')
    parser.add_argument('--port', type=int, default=9443)
    parser.add_argument('--state', type=Path, default=Path(os.environ.get('LOCALAPPDATA', str(Path.home()))) / 'VibDock/service.json')
    parser.add_argument('--allow-action', action='append', default=[], help='Saved action id; repeat to allow more')
    parser.add_argument('--allow-microphone', action='store_true', help='Allow toggling the exact microphone saved in Settings')
    parser.add_argument('--stt-exe', type=Path)
    parser.add_argument('--model-dir', type=Path)
    args = parser.parse_args()
    ip = ipaddress.ip_address(args.listen)
    if ip.version != 4 or not ip.is_private or ip.is_loopback or ip.is_unspecified or ip.is_multicast or ip.is_link_local:
        parser.error('Select the laptop private Wi-Fi IPv4 address.')
    if not args.allow_action and not args.allow_microphone and not args.stt_exe:
        parser.error('Explicitly allow an action, microphone toggle, or configured speech engine.')
    if bool(args.stt_exe) != bool(args.model_dir):
        parser.error('--stt-exe and --model-dir must be provided together.')
    bridge = Bridge(args.state, args.allow_action, args.allow_microphone, args.stt_exe, args.model_dir)
    bridge.approve()  # Prove target and freeze explicit approvals before opening the listener.
    directory = args.state.parent / 'phone-preview'
    cert, key = certificates(directory, args.listen)
    host = f'{args.listen}:{args.port}'
    phone_url = f'https://{host}/#{bridge.ticket}'
    remote = Server((args.listen, args.port), handler(bridge, host))
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(cert, key)
    # TLS handshakes run in bounded workers, not the accept loop.
    original = remote.finish_request
    def finish(request, address):
        with context.wrap_socket(request, server_side=True) as secured:
            original(secured, address)
    remote.finish_request = finish
    secret = secrets.token_urlsafe(32)
    panel = Server(('127.0.0.1', 0), BaseHTTPRequestHandler)
    panel_host = f'127.0.0.1:{panel.server_port}'
    panel.RequestHandlerClass = handler(bridge, panel_host, True, secret, phone_url, directory / 'phone-ca.cer')
    threading.Thread(target=panel.serve_forever, daemon=True).start()
    import webbrowser
    url = f'http://{panel_host}/{secret}'
    print(f'Target: {bridge.target}\nPairing panel (laptop only): {url}\nCtrl+C revokes all sessions. No firewall changes are made.')
    webbrowser.open(url)
    try:
        remote.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        with bridge.lock:
            if bridge.process:
                bridge.process[1].kill()
            bridge.sessions.clear()
        remote.server_close()
        panel.shutdown()
        panel.server_close()


if __name__ == '__main__':
    main()
