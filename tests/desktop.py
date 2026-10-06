#!/usr/bin/env python3
"""Desktop lifecycle contract, with isolated data; no native GUI or real provider needed."""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

root = Path(__file__).resolve().parents[1]
binary = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else root / 'target/release/customremote'
with tempfile.TemporaryDirectory(prefix='customremote-desktop-') as directory:
    data = Path(directory)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]
    env = {**os.environ, 'REMOTE_DATA': directory, 'REMOTE_PORT': str(port), 'REMOTE_HOST': '127.0.0.1', 'REMOTE_SYSTEM_ACCOUNTS': '', 'REMOTE_ENABLE_SESSIONS': '0', 'REMOTE_V1_ALLOW_MASTER': '0'}
    def setup(payload, *extra, expected=0):
        result = subprocess.run([str(binary), 'setup', '--json-stdin', *extra], input=payload, env=env, capture_output=True, timeout=10)
        assert result.returncode == expected, result.stderr
        assert b'desktop-password-2026' not in result.stdout + result.stderr
    setup(b'{"username":"admin","password":"short"}', expected=1)
    assert not (data / 'admin.json').exists()
    setup(b'invalid-json', expected=1)
    setup(b'x' * 4097, expected=1)
    setup(b'{"username":"admin","password":"desktop-password-2026"}')
    original = (data / 'admin.json').read_bytes()
    setup(b'{"username":"changed","password":"another-password-2026"}', '--if-missing')
    assert (data / 'admin.json').read_bytes() == original
    assert (data / 'admin.json').stat().st_mode & 0o077 == 0
    print('PASS: private stdin setup, validation, no secret output, existing account preserved')
    def start():
        process = subprocess.Popen([str(binary), 'serve', '--exit-on-stdin-close'], stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env=env, cwd=root)
        for _ in range(100):
            try:
                with urllib.request.urlopen(f'http://127.0.0.1:{port}/healthz', timeout=.5) as response:
                    assert response.status == 200
                return process
            except OSError:
                if process.poll() is not None:
                    raise RuntimeError(process.stderr.read().decode())
                time.sleep(.05)
        process.kill(); process.wait(); raise RuntimeError('Server did not start')
    p = start()
    try:
        req = urllib.request.Request(f'http://127.0.0.1:{port}/admin/auth/password', data=b'{"username":"admin","password":"desktop-password-2026"}', headers={'Content-Type': 'application/json', 'X-Admin': '1'})
        with urllib.request.urlopen(req, timeout=5) as response:
            assert response.status == 200 and 'Set-Cookie' in response.headers
        p.stdin.close()
        assert p.wait(timeout=8) == 0, p.stderr.read()
        print('PASS: real password login and graceful shutdown when parent pipe closes')
    finally:
        if p.poll() is None: p.kill(); p.wait()
    p = start()
    try:
        p.send_signal(signal.SIGTERM)
        assert p.wait(timeout=8) == 0
        assert (data / 'admin.json').read_bytes() == original
        print('PASS: restart retains admin; SIGTERM also exits with parent pipe still open')
    finally:
        p.stdin.close()
        if p.poll() is None: p.kill(); p.wait()
