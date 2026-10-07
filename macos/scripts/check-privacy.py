#!/usr/bin/env python3
"""Inspect a built bundle without printing private search terms.

Optional private terms belong in a local JSON array outside the repository.
The scan is a release check, not a proof that arbitrary third-party binaries are safe.
"""
import argparse
import mmap
from pathlib import Path
import json
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('app', type=Path)
parser.add_argument('--private-terms-file', type=Path)
args = parser.parse_args()
app = args.app.resolve()
terms = {str(Path.home()), str(Path.cwd())}
if args.private_terms_file:
    extra = json.loads(args.private_terms_file.read_text())
    if not isinstance(extra, list) or not all(isinstance(s, str) and len(s) >= 4 for s in extra):
        parser.error('Private terms must be a JSON array of strings at least four characters long')
    terms.update(extra)
patterns = {t.encode(encoding) for t in terms for encoding in ('utf-8', 'utf-16-le')}
forbidden = {'admin.json', 'accounts.json', 'accounts_state.json', 'api_keys.json',
             'auth.json', 'oauth_creds.json', 'token', '.env', '.DS_Store', 'server.log'}
errors = []
count = 0
for path in sorted(app.rglob('*')):
    relative = str(path.relative_to(app))
    if path.is_symlink():
        errors.append(f'{relative}: unexpected symlink')
        continue
    if not path.is_file():
        continue
    count += 1
    if path.name in forbidden or path.suffix in {'.p12', '.pfx', '.pem', '.key', '.mobileprovision'}:
        errors.append(f'{relative}: unexpected data or credential file')
    if path.stat().st_size:
        with path.open('rb') as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as data:
            if any(data.find(pattern) >= 0 for pattern in patterns):
                errors.append(f'{relative}: private text detected')
attributes = subprocess.check_output(['xattr', '-r', str(app)], text=True)
if any(name in attributes for name in ('com.apple.quarantine', 'com.apple.metadata:kMDItemWhereFroms',
                                     'com.apple.metadata:kMDItemDownloadedDate')):
    errors.append('Bundle contains download provenance metadata')
if errors:
    raise SystemExit('\n'.join(errors))
print(f'PASS: {count} bundle files checked for private paths, supplied terms, credentials and download metadata')
