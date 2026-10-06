#!/usr/bin/env python3
"""Exercise the actual DMG, a relocated copy, native WebKit, and bundled HTTP/PDF tools.

Uses temporary data and mock provider processes; never modifies a user's installed app.
"""
import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import socket
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser()
parser.add_argument('dmg', type=Path)
parser.add_argument('--report', type=Path, default=Path('/tmp/customremote-dmg-test.json'))
args = parser.parse_args()
dmg = args.dmg.resolve()
report = {'dmg': str(dmg), 'sha256': hashlib.sha256(dmg.read_bytes()).hexdigest(), 'checks': []}


def run(label, command, timeout=90, allowed=(0,)):
    started = time.monotonic()
    result = subprocess.run([str(v) for v in command], text=True, capture_output=True, timeout=timeout)
    entry = {'name': label, 'exit_code': result.returncode, 'seconds': round(time.monotonic()-started, 2), 'output': (result.stdout + result.stderr).strip()}
    report['checks'].append(entry)
    args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + '\n')
    if result.returncode not in allowed:
        raise RuntimeError(f'{label}: {entry["output"]}')
    print('PASS:', label, flush=True)
    return result


run('DMG checksum', ['hdiutil', 'verify', dmg])
with tempfile.TemporaryDirectory(prefix='customremote-dmg-') as directory:
    temp = Path(directory)
    mount = temp / 'Volume'
    destination = temp / 'Applications with spaces' / 'PocketRelay.app'
    mounted = False
    try:
        result = run('Read-only DMG mount', ['hdiutil', 'attach', '-readonly', '-nobrowse', '-mountpoint', mount, '-plist', dmg])
        mounted = True
        info = plistlib.loads(result.stdout.encode())
        assert any(v.get('mount-point') and Path(v['mount-point']).resolve() == mount.resolve() for v in info['system-entities'])
        assert (mount / 'Applications').is_symlink()
        assert (mount / 'Applications').readlink() == Path('/Applications')
        assert (mount / 'Read me.txt').is_file()
        assert (mount / 'PocketRelay.app/Contents/MacOS/PocketRelay').is_file()
        run('Copy app from mounted DMG', ['ditto', mount / 'PocketRelay.app', destination])
        run('Eject DMG before launching copied app', ['hdiutil', 'detach', mount])
        mounted = False
        run('Relocated app signature', ['codesign', '--verify', '--deep', '--strict', destination])
        result = run('Gatekeeper distribution assessment (recorded separately)', ['spctl', '--assess', '--type', 'execute', '--verbose=2', destination], allowed=(0, 3))
        report['gatekeeper_accepted'] = result.returncode == 0
        if not report['gatekeeper_accepted']:
            print('LIMIT: Gatekeeper rejects this local ad-hoc build; Developer ID/notarization still required.', flush=True)
        run('Bundled executables without Homebrew PATH', ['python3', ROOT / 'macos/scripts/verify.py', destination])
        with socket.socket() as probe:
            probe.settimeout(1)
            assert probe.connect_ex(('127.0.0.1', 18789)) != 0, 'Test port still has a listener'
        result = run('Native app WebKit login, cookie, API key and API', [destination / 'Contents/MacOS/PocketRelay', '--smoke-test'], timeout=45)
        assert 'PASS: native setup, owned server, WebKit login/cookie/key/API' in result.stdout
        with socket.socket() as probe:
            probe.settimeout(1)
            assert probe.connect_ex(('127.0.0.1', 18789)) != 0, 'Test port still has a listener'
        report['native_service_stopped'] = True
        binary = destination / 'Contents/Resources/bin/customremote'
        run('Desktop setup, persistence and parent-process lifecycle', ['python3', ROOT / 'tests/desktop.py', binary])
        # The integration suite inherits this PATH for PDF commands; providers themselves are fixtures.
        import os
        previous = os.environ.get('PATH', '')
        os.environ['PATH'] = str(binary.parent) + ':' + previous
        try:
            run('36 HTTP/provider-mock checks including bundled PDFKit', ['python3', ROOT / 'tests/integration.py', binary], timeout=120)
        finally:
            os.environ['PATH'] = previous
        report['functional_tests_passed'] = True
    finally:
        if mounted:
            subprocess.run(['hdiutil', 'detach', str(mount)], check=True)
        args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + '\n')
print('Report:', args.report, flush=True)
