#!/usr/bin/env python3
"""Check public Cargo.lock package versions against current OSV advisories."""
import json
from pathlib import Path
import time
import tomllib
import urllib.error
import urllib.request

packages = tomllib.loads((Path(__file__).resolve().parents[1] / 'Cargo.lock').read_text())['package']
queries = [{'package': {'name': p['name'], 'ecosystem': 'crates.io'}, 'version': p['version']}
           for p in packages if p.get('source', '').startswith('registry+')]
request = urllib.request.Request('https://api.osv.dev/v1/querybatch',
                                 data=json.dumps({'queries': queries}).encode(),
                                 headers={'Content-Type': 'application/json'})
for attempt in range(3):
    try:
        with urllib.request.urlopen(request, timeout=45) as response:
            result = json.load(response)
        break
    except (urllib.error.URLError, TimeoutError):
        if attempt == 2:
            raise SystemExit('Dependency advisory lookup unavailable; retry the check.')
        time.sleep(2)
assert len(result['results']) == len(queries), 'Incomplete advisory lookup'
findings = [(q, r['vulns']) for q, r in zip(queries, result['results']) if r.get('vulns')]
for query, advisories in findings:
    print(query['package']['name'], query['version'], ', '.join(a['id'] for a in advisories))
if findings:
    raise SystemExit('Known dependency advisories found; review and update before release.')
print(f'PASS: {len(queries)} locked registry dependencies checked; no known OSV advisories found.')
