#!/usr/bin/env python3
"""Run the coordinator and an opt-in, independently privileged rental canary.

Only root performs bootstrap. The runner never receives coordinator keys or
administrative credentials. A missing or changed journal never resets a pilot.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.parse
import urllib.request

ROOT = Path('/data/canary-private')
PUBLIC = Path('/data/canary-public')
BIN = Path('/opt/covenant/bin')
CACHE = Path('/data/canary-tools')
RUNNER = '/opt/covenant/scripts/compute-rental-canary.py'
VERSION = '0.3.1'
WINDOW = 21600
MAX_ARCHIVE = 256 * 1024 * 1024
ASSET = 'covenant-compute-linux-x86_64'
URL = f'https://github.com/open-covenant/covenant/releases/download/compute-v{VERSION}/{ASSET}.tar.gz'
PUBLIC_HEALTH = 'https://compute-api.opencovenant.org/health'
HEALTH_INTERVAL = 5
HEALTH_TIMEOUT = 3
MAX_HEALTH = 4096


def require(value):
    if not value:
        raise ValueError('canary bootstrap refused')


def atomic(path, data, mode=0o600, owner=None):
    fd, tmp = tempfile.mkstemp(prefix='.' + path.name, dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as stream:
            os.fchmod(stream.fileno(), mode)
            if owner is not None:
                os.fchown(stream.fileno(), owner.pw_uid, owner.pw_gid)
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(tmp, path)
        fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


def write_once(path, data, owner):
    require(not path.is_symlink())
    if path.exists():
        require(path.is_file() and path.read_bytes() == data)
    else:
        atomic(path, data, owner=owner)
    os.chmod(path, 0o600)
    os.chown(path, owner.pw_uid, owner.pw_gid)


def redirect_allowed(url):
    parsed = urllib.parse.urlsplit(url)
    return (parsed.scheme == 'https' and parsed.hostname in
            {'github.com', 'release-assets.githubusercontent.com'}
            and parsed.port in (None, 443) and parsed.username is None
            and parsed.password is None)


class ReleaseRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        require(redirect_allowed(newurl))
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def install_archive(archive, digest, destination):
    require(re.fullmatch('[0-9a-f]{64}', digest))
    require(archive.stat().st_size <= MAX_ARCHIVE)
    with archive.open('rb') as stream:
        actual = hashlib.sha256()
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            actual.update(chunk)
        require(actual.hexdigest() == digest)
    # Do not extract paths, links, permissions or unrelated release executables.
    with tarfile.open(archive, 'r:gz') as bundle:
        members = bundle.getmembers()
        require(len(members) <= 16)
        selected = [m for m in members if m.name == ASSET + '/covenant-compute']
        require(len(selected) == 1 and selected[0].isfile()
                and 0 < selected[0].size <= 128 * 1024 * 1024)
        for member in members:
            require(not member.issym() and not member.islnk()
                    and not member.name.startswith('/') and '..' not in member.name.split('/'))
        with bundle.extractfile(selected[0]) as stream:
            data = stream.read(128 * 1024 * 1024 + 1)
        require(len(data) == selected[0].size)
        atomic(destination, data, 0o555)


def require_cache_metadata(path, directory=False):
    info = path.lstat()
    require(info.st_uid == os.geteuid() and info.st_mode & 0o022 == 0)
    if directory:
        require(stat.S_ISDIR(info.st_mode))
    else:
        require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1
                and 0 < info.st_size <= MAX_ARCHIVE)


def cache_archive(source, target):
    # Stream the already-verified archive instead of allocating a second full
    # compressed bundle beside the coordinator in the 512MiB service.
    fd, temporary = tempfile.mkstemp(prefix='.release-', dir=target.parent)
    try:
        with os.fdopen(fd, 'wb') as output, source.open('rb') as stream:
            total = 0
            while chunk := stream.read(1024 * 1024):
                total += len(chunk)
                require(total <= MAX_ARCHIVE)
                output.write(chunk)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, target)
        fd = os.open(target.parent, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def install_client(env):
    digest = env.get('COVENANT_CANARY_RELEASE_SHA256', '')
    require(re.fullmatch('[0-9a-f]{64}', digest))
    BIN.mkdir(parents=True, exist_ok=True)
    os.chmod(BIN, 0o755)
    require(not CACHE.is_symlink())
    CACHE.mkdir(mode=0o700, parents=True, exist_ok=True)
    require_cache_metadata(CACHE, directory=True)
    os.chmod(CACHE, 0o700)
    cached = CACHE / (VERSION + '-' + digest + '.tar.gz')
    require(not cached.is_symlink())
    if cached.exists():
        # A verified root-only persistent archive permits recovery during a
        # GitHub outage. Rehash on every boot; corruption is not silently reset.
        require_cache_metadata(cached)
        install_archive(cached, digest, BIN / 'covenant-compute')
        verify_client_version()
        return
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), ReleaseRedirect())
    request = urllib.request.Request(URL, headers={'User-Agent': 'Covenant-rental-canary/0.3.1',
                                                   'Accept-Encoding': 'identity'})
    with tempfile.TemporaryDirectory() as temporary:
        archive = Path(temporary) / 'release.tar.gz'
        with opener.open(request, timeout=30) as response, archive.open('wb') as output:
            require(response.status == 200 and redirect_allowed(response.geturl()))
            require(response.headers.get('Content-Encoding', 'identity') == 'identity')
            total = 0
            while chunk := response.read(1024 * 1024):
                total += len(chunk)
                require(total <= MAX_ARCHIVE)
                output.write(chunk)
        install_archive(archive, digest, BIN / 'covenant-compute')
        cache_archive(archive, cached)
    # No buyer identity or network is touched by --version.
    verify_client_version()


def verify_client_version():
    got = subprocess.check_output([str(BIN / 'covenant-compute'), '--version'], timeout=10,
                                  env={'PATH': '/usr/bin:/bin'}, text=True).strip()
    require(got == 'covenant-compute ' + VERSION)


def environments(env):
    coordinator = {k: v for k, v in env.items() if not k.startswith('COVENANT_CANARY_')}
    canary = {'PATH': '/usr/local/bin:/usr/bin:/bin', 'LANG': 'C.UTF-8',
              'HOME': str(ROOT), 'COVENANT_CANARY_STATE_DIR': str(ROOT / 'state')}
    allowed = ('BUYER_PUBKEY', 'VAST_API_KEY', 'ALERT_CLOUDFLARE_API_TOKEN',
               'ALERT_CLOUDFLARE_ACCOUNT_ID', 'ALERT_FROM', 'ALERT_TO')
    for suffix in allowed:
        key = 'COVENANT_CANARY_' + suffix
        if key in env:
            canary[key] = env[key]
    coordinator['COVENANT_COMPUTE_RENTAL_CHECKS_FILE'] = str(PUBLIC / 'state.json')
    return coordinator, canary


def prepare(env, owner):
    require(ROOT.parent == Path('/data') and not ROOT.is_symlink() and not PUBLIC.is_symlink())
    for path in (ROOT, ROOT / 'buyer', ROOT / 'ssh', ROOT / 'state', PUBLIC):
        require(not path.is_symlink())
        path.mkdir(parents=True, exist_ok=True)
        os.chmod(path, 0o755 if path == PUBLIC else 0o700)
        os.chown(path, owner.pw_uid, owner.pw_gid)
    seed = base64.b64decode(env['COVENANT_CANARY_BUYER_SEED_BASE64'], validate=True)
    require(len(seed) == 32)
    # No key creation on a restart or if Render omits a secret.
    write_once(ROOT / 'buyer/identity.json', seed, owner)
    private = base64.b64decode(env['COVENANT_CANARY_SSH_PRIVATE_KEY_BASE64'], validate=True)
    public = env['COVENANT_CANARY_SSH_PUBLIC_KEY'].strip() + '\n'
    require(private.startswith(b'-----BEGIN OPENSSH PRIVATE KEY-----\n'))
    require(public.startswith('ssh-ed25519 ') and len(private) < 4096 and len(public) < 1024)
    write_once(ROOT / 'ssh/id_ed25519', private, owner)
    write_once(ROOT / 'ssh/id_ed25519.pub', public.encode(), owner)
    config = json.loads(Path('/opt/covenant/config.example.json').read_text())
    config.update(cli=[str(BIN / 'covenant-compute')], buyer_home=str(ROOT / 'buyer'),
                  ssh_private_key=str(ROOT / 'ssh/id_ed25519'),
                  ssh_public_key=str(ROOT / 'ssh/id_ed25519.pub'),
                  rpc_url=env['COVENANT_CANARY_RPC_URL'],
                  evidence_command=['/usr/bin/python3', '/opt/covenant/scripts/compute-canary-vast.py'],
                  alert_command=['/usr/bin/python3', '/opt/covenant/scripts/compute-canary-alert.py'],
                  public_state_path=str(PUBLIC / 'state.json'))
    atomic(ROOT / 'config.json', (json.dumps(config, sort_keys=True) + '\n').encode(), owner=owner)
    # Persist the installation identity separately from monetary journals.
    pin = {'version': VERSION, 'archive_sha256': env['COVENANT_CANARY_RELEASE_SHA256'],
           'buyer_pubkey': env['COVENANT_CANARY_BUYER_PUBKEY'],
           'seed_sha256': hashlib.sha256(seed).hexdigest()}
    write_once(ROOT / 'installation.json', (json.dumps(pin, sort_keys=True) + '\n').encode(), owner)


def verify_identity(env, owner):
    isolated = {'PATH': '/usr/bin:/bin', 'COVENANT_COMPUTE_MCP_HOME': str(ROOT / 'buyer')}
    identity = json.loads(subprocess.check_output([str(BIN / 'covenant-compute'), 'whoami', '--json'],
        env=isolated, user=owner.pw_uid, group=owner.pw_gid, extra_groups=[], timeout=10))
    require(identity.get('pubkey') == env['COVENANT_CANARY_BUYER_PUBKEY'])
    derived = subprocess.check_output(['/usr/bin/ssh-keygen', '-y', '-f', str(ROOT / 'ssh/id_ed25519')],
        env=isolated, user=owner.pw_uid, group=owner.pw_gid, extra_groups=[], timeout=10, text=True)
    require(derived.split()[:2] == (ROOT / 'ssh/id_ed25519.pub').read_text().split()[:2])


def unresolved(state):
    # Never allow a corrupt/missing ledger to become a fresh paid start.
    if (state / 'active.json').exists():
        return True
    try:
        ledger = json.loads((state / 'budget.json').read_text())
        return any(e.get('resolved_at_unix') is None for e in ledger['entries'])
    except FileNotFoundError:
        return (state / 'budget-initialized').exists()
    except Exception:
        return True


class NoHealthRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def local_health_url(env):
    port = env.get('COVENANT_COMPUTE_COORDINATOR_PORT', '8720')
    require(isinstance(port, str) and re.fullmatch('[0-9]{1,5}', port)
            and 1 <= int(port) <= 65535)
    return 'http://127.0.0.1:' + str(int(port)) + '/health'


def health_revision(url):
    require(url == PUBLIC_HEALTH or re.fullmatch(r'http://127\.0\.0\.1:[0-9]{1,5}/health', url))
    request = urllib.request.Request(url, headers={
        'User-Agent': 'Covenant-Rental-Canary/0.3.1', 'Accept': 'application/json',
        'Accept-Encoding': 'identity', 'Cache-Control': 'no-cache'})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoHealthRedirect())

    def expired(*_):
        raise TimeoutError('coordinator readiness timeout')

    # The socket timeout alone is not a total deadline for a slowly streamed
    # response. This supervisor runs on Unix in its main thread; bound headers,
    # TLS and the complete body too, without affecting child processes.
    previous = signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, HEALTH_TIMEOUT)
    try:
        with opener.open(request, timeout=HEALTH_TIMEOUT) as response:
            require(response.status == 200 and response.geturl() == url)
            require(response.headers.get('Content-Type', '').split(';')[0].strip() == 'application/json'
                    and response.headers.get('Content-Encoding', 'identity') == 'identity')
            length = response.headers.get('Content-Length')
            require(length is None or re.fullmatch('[0-9]+', length) and int(length) <= MAX_HEALTH)
            data = response.read(MAX_HEALTH + 1)
            require(len(data) <= MAX_HEALTH and (length is None or len(data) == int(length)))
            value = json.loads(data.decode('utf-8'))
            require(isinstance(value, dict) and value.get('status') == 'ok'
                    and re.fullmatch('[0-9a-f]{40}', value.get('revision', '')))
            return value['revision']
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


class AdmissionReadiness:
    """Two recent observations before new work; never a recovery prerequisite."""
    def __init__(self, env):
        self.env = env
        self.next_probe = 0
        self.last_probe = None
        self.revision = None
        self.observations = 0

    def ready(self):
        now = time.monotonic()
        if now < self.next_probe:
            return self.observations >= 2
        if self.last_probe is None or now - self.last_probe > 2 * HEALTH_INTERVAL:
            self.revision, self.observations = None, 0
        self.last_probe = now
        self.next_probe = now + HEALTH_INTERVAL
        try:
            local = health_revision(local_health_url(self.env))
            require(health_revision(PUBLIC_HEALTH) == local)
        except Exception:
            self.revision, self.observations = None, 0
            return False
        self.observations = self.observations + 1 if self.revision == local else 1
        self.revision = local
        return self.observations >= 2


def scheduling_action(mode, journal, pending, now):
    """Pure decision; markers are synced by caller BEFORE starting a subprocess."""
    if pending:
        return 'recover' if now - journal.get('recovery_attempt_at', 0) >= 300 else None
    if mode == 'preflight':
        return 'preflight' if not journal.get('preflight_attempted') else None
    if mode == 'once':
        return 'rent' if not journal.get('once_attempted') else None
    if mode == 'scheduled' and journal.get('last_window', -1) < now // WINDOW:
        return 'rent'
    return None


def read_schedule(path, state):
    """Missing/corrupt scheduler state cannot renew a completed one-shot grant."""
    require(not path.is_symlink())
    if not path.exists():
        require(not any((state / name).exists() for name in ('budget.json', 'budget-initialized', 'active.json')))
        return {'schema': 1}
    require(path.is_file() and path.stat().st_size <= 16384)
    journal = json.loads(path.read_text())
    require(isinstance(journal, dict) and journal.get('schema') == 1)
    for key in ('recovery_attempt_at', 'last_window'):
        if key in journal:
            require(type(journal[key]) is int and journal[key] >= 0)
    for key in ('preflight_attempted', 'once_attempted'):
        if key in journal:
            require(type(journal[key]) is bool)
    return journal


def spawn(argv, env, owner):
    return subprocess.Popen(argv, env=env, user=owner.pw_uid, group=owner.pw_gid,
                            extra_groups=[], start_new_session=True,
                            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL)


def main():
    mode = os.environ.get('COVENANT_CANARY_MODE', 'disabled')
    require(mode in {'disabled', 'preflight', 'once', 'scheduled'})
    coordinator_owner, owner = pwd.getpwnam('covenant'), pwd.getpwnam('covenant-canary')
    coordinator_env, canary_env = environments(os.environ)
    admission = AdmissionReadiness(coordinator_env)
    coordinator = subprocess.Popen(['covenant-compute-coordinator', *sys.argv[1:]],
        env=coordinator_env, user=coordinator_owner.pw_uid, group=coordinator_owner.pw_gid,
        extra_groups=[], start_new_session=True)
    children = [coordinator]
    stopping = False
    def stop(*_):
        nonlocal stopping
        stopping = True
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    ready, worker, monitor = False, None, None
    next_bootstrap, next_monitor = 0, 0
    journal_path = ROOT / 'supervisor.json'
    state = ROOT / 'state'
    try:
        while not stopping and coordinator.poll() is None:
            now = int(time.time())
            if not ready and now >= next_bootstrap:
                try:
                    install_client(os.environ)
                    prepare(os.environ, owner)
                    verify_identity(os.environ, owner)
                    ready = True
                    print('rental canary installed and isolated', flush=True)
                except Exception:
                    print('rental canary bootstrap unavailable; no paid dispatch', flush=True)
                    next_bootstrap = now + 300
            if ready:
                args = ['/usr/bin/python3', RUNNER, '--config', str(ROOT / 'config.json'),
                        '--state-dir', str(state)]
                if now >= next_monitor and (monitor is None or monitor.poll() is not None):
                    monitor = spawn(args + ['--monitor'] + ([] if mode == 'scheduled' else ['--disabled']), canary_env, owner)
                    children.append(monitor)
                    next_monitor = now + 60
                if worker is None or worker.poll() is not None:
                    try:
                        pending = unresolved(state)
                        try:
                            journal = read_schedule(journal_path, state)
                            write_path = journal_path
                        except Exception:
                            # Admission history stays untouched on corruption. A
                            # separate recovery-only clock cannot authorize rent.
                            require(pending)
                            write_path = ROOT / 'recovery-supervisor.json'
                            journal = json.loads(write_path.read_text()) if write_path.exists() else {'schema': 1}
                            require(type(journal.get('recovery_attempt_at', 0)) is int)
                        action = scheduling_action(mode, journal, pending, now)
                        # A released client can be installed before Render has
                        # routed the public endpoint to this coordinator. Do
                        # not consume a one-shot/window grant during that gap.
                        # Recovery and the separately spawned monitor bypass it.
                        if action in {'preflight', 'rent'}:
                            if not admission.ready() or stopping or coordinator.poll() is not None:
                                action = None
                            else:
                                # Probes may cross a UTC scheduling boundary.
                                now = int(time.time())
                                action = scheduling_action(mode, journal, pending, now)
                        if action:
                            if action == 'recover': journal['recovery_attempt_at'] = now
                            elif action == 'preflight': journal['preflight_attempted'] = True
                            else:
                                journal['once_attempted'] = True
                                journal['last_window'] = now // WINDOW
                            atomic(write_path, (json.dumps(journal) + '\n').encode(), owner=owner)
                            flag = '--recover-only' if action == 'recover' else '--preflight' if action == 'preflight' else '--once'
                            worker = spawn(args + [flag], canary_env, owner)
                            children.append(worker)
                    except Exception:
                        print('rental canary scheduling unavailable; no new dispatch', flush=True)
                children = [p for p in children if p.poll() is None]
                if coordinator not in children: children.insert(0, coordinator)
            time.sleep(1)
    finally:
        # Signal process groups, including SSH/CLI. Durable unknown outcomes are
        # retained for the next instance; coordinator settlement resumes normally.
        for child in children:
            if child.poll() is None:
                try: os.killpg(child.pid, signal.SIGTERM)
                except ProcessLookupError: pass
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline and any(p.poll() is None for p in children):
            time.sleep(0.2)
        for child in children:
            if child.poll() is None:
                try: os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError: pass
    return coordinator.returncode or 0


if __name__ == '__main__':
    sys.exit(main())
