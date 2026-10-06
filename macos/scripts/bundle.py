#!/usr/bin/env python3
"""Bundle standalone provider CLIs; PDF handling uses the system PDFKit framework."""
import hashlib
import json
import os
import plistlib
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import urllib.request

resources = Path(sys.argv[1]).resolve()
arch = subprocess.check_output(['uname', '-m'], text=True).strip()
bindir = resources / 'bin'
libdir = resources.parent / 'Frameworks'
licenses = resources / 'ThirdParty'
for d in (bindir, libdir, licenses):
    d.mkdir(parents=True, exist_ok=True)
manifest = []
seen = {}
minimum_macos = [(14, 0)]


def run(*args):
    return subprocess.check_output(args, text=True)


def expanded(path, source):
    return path.replace('@loader_path', str(source.parent)).replace('@executable_path', str(source.parent))


def copy_binary(source, dest):
    source = source.resolve()
    if source in seen:
        return seen[source]
    if arch not in run('lipo', '-archs', str(source)).split():
        raise RuntimeError(f'{source}: architecture {arch} absente')
    seen[source] = dest
    shutil.copy2(source, dest)
    dest.chmod(0o755)
    manifest.append({'file': str(dest.relative_to(resources.parent)), 'source_filename': source.name, 'sha256_before_relocation': hashlib.sha256(source.read_bytes()).hexdigest()})
    subprocess.run(['codesign', '--remove-signature', str(dest)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    # Homebrew puts licenses beside the bin/lib directories in the versioned Cellar.
    for parent in source.parents:
        if parent.parent.parent.name == 'Cellar' or parent.name == 'vendor':
            target = licenses / parent.parent.name
            target.mkdir(exist_ok=True)
            for pattern in ('LICENSE*', 'COPYING*', 'NOTICE*', 'AUTHORS*'):
                for file in parent.glob(pattern):
                    if file.is_file():
                        shutil.copy2(file, target / file.name)
            break
    load = run('otool', '-l', str(source))
    versions = re.findall(r'\bminos (\d+\.\d+(?:\.\d+)?)', load) + re.findall(r'cmd LC_VERSION_MIN_MACOSX\n\s+cmdsize \d+\n\s+version (\d+\.\d+(?:\.\d+)?)', load)
    minimum_macos.extend(tuple(map(int, v.split('.'))) for v in versions)
    rpaths = [expanded(p, source) for p in re.findall(r'cmd LC_RPATH\n\s+cmdsize \d+\n\s+path (.+?) \(offset', load)]
    deps = [line.strip().split(' (compatibility')[0] for line in run('otool', '-L', str(source)).splitlines()[1:]]
    own_id = run('otool', '-D', str(source)).splitlines()[1:]
    for dep in deps:
        if dep in own_id or dep.startswith(('/usr/lib/', '/System/Library/')):
            continue
        if dep.startswith('@rpath/'):
            options = [Path(p) / dep[len('@rpath/'):] for p in rpaths]
            dependency = next((p for p in options if p.is_file()), None)
            if dependency is None:
                raise RuntimeError(f'Cannot resolve {dep} from {source}')
        else:
            dependency = Path(expanded(dep, source))
        destination = libdir / dependency.name
        if destination.exists() and dependency.resolve() not in seen:
            raise RuntimeError(f'Dylib name collision: {dependency}')
        copied = copy_binary(dependency, destination)
        replacement = '@loader_path/' + os.path.relpath(copied, dest.parent)
        subprocess.run(['install_name_tool', '-change', dep, replacement, str(dest)], check=True)
    if dest.suffix == '.dylib':
        subprocess.run(['install_name_tool', '-id', '@rpath/' + dest.name, str(dest)], check=True)
    # No Homebrew path is required at runtime, even if the user's machine has no brew.
    for rpath in re.findall(r'cmd LC_RPATH\n\s+cmdsize \d+\n\s+path (.+?) \(offset', load):
        if rpath.startswith(('/opt/homebrew', '/usr/local')):
            subprocess.run(['install_name_tool', '-delete_rpath', rpath, str(dest)], check=True)
    return dest


for name in ['claude', 'codex']:
    source = os.environ.get('MACOS_' + name.upper() + '_BIN') or shutil.which(name)
    if not source:
        raise SystemExit(f'{name} is missing. Install the build dependencies listed in docs/macos.md.')
    copy_binary(Path(source), bindir / name)

# Antigravity doesn't have to be installed on the developer's Mac.
source = os.environ.get('MACOS_ANTIGRAVITY_BIN') or shutil.which('antigravity')
if not source:
    flavor = 'arm64' if arch == 'arm64' else 'x64'
    checksums = {'arm64': '95d5d8ab8870b849a157f647bb4d9953184f97855cbfbedebdedcc421ca5b435',
                 'x64': '0b09a1d3a8c0df10a1090f99fb120eeff007dee53e7853f241a373f0194d5ca7'}
    cache = Path.home() / 'Library/Caches/PocketRelay-build' / flavor
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / 'antigravity-1.2.10.tar.gz'
    if not archive.exists():
        url = f'https://github.com/google-antigravity/antigravity-cli/releases/download/1.2.10/agy_cli_mac_{flavor}.tar.gz'
        with urllib.request.urlopen(url, timeout=120) as response:
            content = response.read()
        if hashlib.sha256(content).hexdigest() != checksums[flavor]:
            raise RuntimeError('Antigravity checksum mismatch')
        archive.write_bytes(content)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != checksums[flavor]:
        raise RuntimeError('Cached Antigravity checksum mismatch')
    with tarfile.open(archive) as tar:
        members = [m for m in tar.getmembers() if m.isfile() and Path(m.name).name == 'antigravity']
        if len(members) != 1:
            raise RuntimeError('Antigravity archive does not contain exactly one executable')
        source = cache / 'antigravity'
        source.write_bytes(tar.extractfile(members[0]).read())
        source.chmod(0o755)
        for member in tar.getmembers():
            if member.isfile() and re.match(r'(LICENSE|COPYING|NOTICE)', Path(member.name).name, re.I):
                (licenses / ('antigravity-' + Path(member.name).name)).write_bytes(tar.extractfile(member).read())
copy_binary(Path(source), bindir / 'antigravity')
(licenses / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')

print(f'{len(manifest)} native executables/libraries bundled ({arch})')

plist_path = resources.parent / 'Info.plist'
info = plistlib.loads(plist_path.read_bytes())
info['LSMinimumSystemVersion'] = '.'.join(map(str, max(minimum_macos)))
plist_path.write_bytes(plistlib.dumps(info))
print('Minimum macOS for bundled dependencies:', info['LSMinimumSystemVersion'])
