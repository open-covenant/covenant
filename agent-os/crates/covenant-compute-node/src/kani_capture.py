"""Node-owned capture shim; the independently authored runner lives in the image.

All writable paths here are bounded container tmpfs. Full original evidence is
returned before tmpfs disappears, so repository code receives no host write mount.
"""
import base64
import json
import os
from pathlib import Path
import stat
import subprocess
import sys

ROOT = Path('/evidence')
LIMIT = 16 * 1024 * 1024
CONTROL_LIMIT = 64 * 1024
ARTIFACTS = ('result.json', 'stdout.log', 'stderr.log', 'kani-export.json')

def read_file(path, cap):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > cap:
        raise ValueError('invalid or oversized evidence file: ' + path.name)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, 'rb') as stream:
        opened = os.fstat(stream.fileno())
        if (not stat.S_ISREG(opened.st_mode) or opened.st_nlink != 1 or opened.st_size > cap
                or (opened.st_dev, opened.st_ino) != (info.st_dev, info.st_ino)):
            raise ValueError('evidence file changed while opening: ' + path.name)
        data = stream.read(cap + 1)
    if len(data) > cap:
        raise ValueError('evidence limit exceeded')
    return data

try:
    ROOT.mkdir(exist_ok=True)
    with (ROOT / 'control.stdout').open('wb') as out, (ROOT / 'control.stderr').open('wb') as err:
        child = subprocess.run(
            ['/usr/bin/python3', '-I', '/opt/covenant/kani/runner.py', 'run',
             '--plan', '/request/plan.json', '--workdir', '/work', '--outdir', '/evidence'],
            stdin=subprocess.DEVNULL, stdout=out, stderr=err, check=False,
        )
    control_out = read_file(ROOT / 'control.stdout', CONTROL_LIMIT)
    control_err = read_file(ROOT / 'control.stderr', CONTROL_LIMIT)
    artifacts = {}
    total = 0
    for name in ARTIFACTS:
        path = ROOT / name
        if not path.exists() and not path.is_symlink():
            continue
        data = read_file(path, LIMIT - total)
        total += len(data)
        artifacts[name] = base64.b64encode(data).decode('ascii')
    json.dump({'artifacts': artifacts,
               'control_stdout_b64': base64.b64encode(control_out).decode('ascii'),
               'control_stderr_b64': base64.b64encode(control_err).decode('ascii')}, sys.stdout,
              separators=(',', ':'))
    sys.stdout.write('\n')
    sys.exit(child.returncode if 0 <= child.returncode <= 255 else 1)
except Exception as exc:
    json.dump({'capture_error': type(exc).__name__ + ': ' + str(exc)[:512]}, sys.stdout)
    sys.stdout.write('\n')
    sys.exit(1)
