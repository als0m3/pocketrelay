#!/usr/bin/env python3
"""Opt-in macOS network test: official downloads, isolated data, no provider sign-in."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import urllib.error

root = Path(__file__).resolve().parents[1]
binary = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else root / 'target/release/customremote'
if sys.platform != 'darwin':
    raise SystemExit('This optional download test requires macOS.')
with tempfile.TemporaryDirectory(prefix='pocketrelay-tools-') as directory:
    data = Path(directory)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]
    env = {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': directory, 'LANG': 'en_US.UTF-8',
           'REMOTE_DATA': directory, 'REMOTE_STATIC': str(root / 'static'), 'REMOTE_HOST': '127.0.0.1',
           'REMOTE_PORT': str(port), 'REMOTE_MANAGED_TOOLS': str(data / 'tools'),
           'REMOTE_ENABLE_SESSIONS': '0', 'REMOTE_SYSTEM_ACCOUNTS': '', 'REMOTE_V1_ALLOW_MASTER': '0'}
    base = f'http://127.0.0.1:{port}'
    cookie = ''
    def request(path, body=None, auth=True, csrf=True, expected=200):
        headers = {'Content-Type': 'application/json'}
        if auth: headers['Cookie'] = cookie
        if csrf: headers['X-Admin'] = '1'
        req = urllib.request.Request(base + path, data=json.dumps(body).encode() if body is not None else None, headers=headers)
        try:
            response = urllib.request.urlopen(req, timeout=5)
        except urllib.error.HTTPError as e:
            assert e.code == expected, (path,e.code,expected)
            return None
        assert response.status == expected, (path,response.status,expected)
        return json.load(response)
    def start():
        global cookie
        p = subprocess.Popen([str(binary), 'serve', '--exit-on-stdin-close'], env=env, stdin=subprocess.PIPE,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(100):
            try:
                urllib.request.urlopen(base + '/healthz',timeout=.5).close(); break
            except OSError: time.sleep(.05)
        else:
            p.kill();p.wait();raise RuntimeError('Server did not start')
        req = urllib.request.Request(base + '/admin/auth/token', data=json.dumps({'token': (data/'token').read_text()}).encode(), headers={'Content-Type':'application/json','X-Admin':'1'})
        with urllib.request.urlopen(req,timeout=5) as r: cookie = r.headers['Set-Cookie'].split(';')[0]
        return p
    def stop(p):
        p.stdin.close()
        try: assert p.wait(timeout=10) == 0
        except subprocess.TimeoutExpired: p.kill();p.wait();raise
    p = start()
    try:
        request('/admin/api/tools?provider=claude',auth=False,expected=401)
        request('/admin/api/tools',{'provider':'claude'},csrf=False,expected=403)
        request('/admin/api/tools',{'provider':'../../escape'},expected=400)
        assert request('/admin/api/state')['managed_tools']
        for provider in ('claude','codex','antigravity'):
            assert request('/admin/api/tools?provider='+provider)['state']=='missing'
        print('PASS: no eager downloads; authentication, CSRF and provider allowlist',flush=True)
        for provider in ('claude','codex','antigravity'):
            request('/admin/api/tools',{'provider':provider})
            request('/admin/api/tools',{'provider':provider})  # Duplicate clicks join the same job.
            started=time.monotonic()
            while True:
                status=request('/admin/api/tools?provider='+provider)
                assert status['state']!='error', status
                if status['state']=='ready':break
                assert time.monotonic()-started<680,'Download timed out'
                time.sleep(.4)
            executable=data/'tools'/provider
            result=subprocess.run([str(executable),'--version'],env={'PATH':'/usr/bin:/bin','HOME':directory,'DISABLE_AUTOUPDATER':'1'},capture_output=True,text=True,timeout=30)
            assert result.returncode==0,result.stderr
            assert status['version'] in result.stdout,(status,result.stdout)
            if provider=='claude':
                assert not (data/'tools/codex').exists() and not (data/'tools/antigravity').exists()
            print('PASS: verified official download and clean-PATH execution:',provider,result.stdout.strip(),flush=True)
        timestamps={provider:(data/'tools'/provider).stat().st_mtime_ns for provider in ('claude','codex','antigravity')}
    finally: stop(p)
    p=start()
    try:
        for provider,timestamp in timestamps.items():
            assert request('/admin/api/tools',{'provider':provider})['state']=='ready'
            assert (data/'tools'/provider).stat().st_mtime_ns==timestamp
        assert not list((data/'tools').glob('.install-*'))
        print('PASS: installed tools reused after server restart; no partial downloads remain',flush=True)
    finally: stop(p)
