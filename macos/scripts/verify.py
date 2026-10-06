#!/usr/bin/env python3
"""Check the bundle with a clean PATH and no machine-level provider credentials."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
app = Path(sys.argv[1]).resolve()
resources = app / 'Contents/Resources'
bindir = resources / 'bin'
with tempfile.TemporaryDirectory(prefix='customremote-bundle-') as home:
    env = {'HOME': home, 'PATH': str(bindir) + ':/usr/bin:/bin', 'LANG': 'en_US.UTF-8', 'DISABLE_AUTOUPDATER': '1'}
    for tool in ['customremote', 'claude', 'codex', 'antigravity', 'pdftotext', 'pdftoppm', 'pdfinfo']:
        option = '-v' if tool.startswith('pdf') else '--version'
        result = subprocess.run([str(bindir / tool), option], env=env, capture_output=True, text=True, timeout=30)
        if result.returncode:
            raise RuntimeError(f'{tool}: {result.stdout}\n{result.stderr}')
        print(f'PASS: {tool} runs without Homebrew PATH')
    for binary in [*bindir.iterdir(), *(app / 'Contents/Frameworks').glob('*.dylib')]:
        linked = subprocess.check_output(['otool', '-L', str(binary)], text=True)
        assert '/opt/homebrew/' not in linked and '/usr/local/' not in linked, linked
print('PASS: no external Homebrew dylib dependencies')
