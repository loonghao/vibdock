"""Prove the actual isolated service connection without dispatching any action."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
from bridge import Bridge


def main(executable):
    with tempfile.TemporaryDirectory(prefix='vibdock-phone-service-') as temp:
        with socket.socket() as probe:
            probe.bind(('127.0.0.1', 0))
            port=probe.getsockname()[1]
        environment={**os.environ,'LOCALAPPDATA':temp,'VIBDOCK_PORT':str(port)}
        # Only the isolated child gets these values; no installed process is stopped.
        process=subprocess.Popen([executable,'service-start'],env=environment,
                                 stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        try:
            state=Path(temp)/'VibDock/service.json'
            for _ in range(100):
                if state.exists():break
                if process.poll() is not None:raise RuntimeError('Isolated service failed to start')
                time.sleep(.05)
            bridge=Bridge(state,[])
            bridge.approve()
            assert bridge.local('GET','/api/config') is not None
            assert bridge.local('GET','/api/actions').get('bindings') == []
            original=state.read_text()
            changed=json.loads(original);changed['token']='different-fixture'
            state.write_text(json.dumps(changed))
            try:
                try:bridge.local('GET','/api/config')
                except ValueError:pass
                else:raise AssertionError('Changed instance identity was accepted')
            finally:state.write_text(original)
            print('PASS: real isolated service ownership proof and identity-change rejection; no actions dispatched')
        finally:
            subprocess.run([executable,'service-stop'],env=environment,stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL,timeout=10,check=False)
            try:process.wait(timeout=10)
            except subprocess.TimeoutExpired:process.kill();process.wait()


if __name__=='__main__':main(str(Path(sys.argv[1]).resolve()))
