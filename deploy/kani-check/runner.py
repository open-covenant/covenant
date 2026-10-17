#!/usr/bin/env python3
"""Covenant Kani verifier runner (Python standard library only).

Validates a covenant.kani.plan.v1 plan, runs the pinned cargo-kani with a fixed
argv inside a sanitized environment, captures raw evidence as files and
classifies the Kani 0.68.0 JSON export fail-closed into
covenant.kani.result.v1. Resource limits (memory, CPU, network, mounts) are
enforced by the outer node/Docker, not by this module.
"""
import hashlib
import json
import os
import re
import signal
import stat
import subprocess
import sys
import threading
import time

PLAN_SCHEMA = 'covenant.kani.plan.v1'
RESULT_SCHEMA = 'covenant.kani.result.v1'
CARGO_KANI = '/usr/local/cargo/bin/cargo-kani'
KANI_VERSION = '0.68.0'
EXPORT_VERSION = '1.0'
TOOL_PIN = {
    'cargo_kani': CARGO_KANI,
    'kani': KANI_VERSION,
    'cbmc': '6.11.0',
    'toolchain': 'nightly-2026-08-21',
    'rustc': 'rustc 1.100.0-nightly (8925ea358 2026-08-20)',
}
MAX_EVIDENCE_BYTES = 16 * 1024 * 1024
WALL_GRACE_SECS = 300
MAX_STR = 1024
MAX_LIST = 64
MAX_UNWIND = 1000000
EXPORT_NAME = 'kani-export.json'
ARTIFACT_NAMES = ('stdout.log', 'stderr.log', EXPORT_NAME)
OUTCOMES = ('proven', 'refuted', 'timeout', 'out_of_memory', 'tool_error', 'inconclusive')

PLAN_KEYS = {'schema', 'harness', 'manifest_path', 'features', 'tests', 'timeout_secs',
             'unwind', 'expected_outcome', 'expected_assertion', 'required_covers'}
HARNESS_RE = re.compile(r'^[A-Za-z_][A-Za-z0-9_]*(::[A-Za-z_][A-Za-z0-9_]*)*$')
FEATURE_RE = re.compile(r'^[A-Za-z0-9_][A-Za-z0-9_.+-]{0,63}$')
PATH_RE = re.compile(r'^[A-Za-z0-9_][A-Za-z0-9_.-]*(/[A-Za-z0-9_][A-Za-z0-9_.-]*)*$')

UNSAFE_ENV = re.compile(r'^(RUSTC.*|RUSTDOC.*|RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_BUILD_RUSTC.*|'
                        r'CARGO_BUILD_RUSTDOC.*|CARGO_BUILD_RUSTFLAGS|CARGO_TARGET_.*|CARGO_BUILD_TARGET_DIR|'
                        r'CARGO_ALIAS_.*|CARGO_BUILD_JOBS|KANI_.*|CBMC.*)$')
UNSAFE_KANI_FLAGS = {'only-codegen', 'prove-safety-only', 'ignore-global-asm', 'cbmc-args',
                     'no-assert-contracts', 'no-codegen', 'no-assertion-reach-checks'}


def _is_int(v):
    return isinstance(v, int) and not isinstance(v, bool)


def _safe_str(v, name, allow_empty=False):
    if not isinstance(v, str):
        raise ValueError('%s must be a string' % name)
    if len(v) > MAX_STR or '\x00' in v:
        raise ValueError('%s too long or contains NUL' % name)
    if not v and not allow_empty:
        raise ValueError('%s must be nonempty' % name)
    return v


def _str_list(v, name):
    if not isinstance(v, list) or len(v) > MAX_LIST:
        raise ValueError('%s must be a list of at most %d items' % (name, MAX_LIST))
    return v


def validate_plan(plan):
    if not isinstance(plan, dict):
        raise ValueError('plan must be an object')
    extra = set(plan) - PLAN_KEYS
    missing = PLAN_KEYS - set(plan)
    if extra:
        raise ValueError('unknown plan keys: %s' % sorted(map(str, extra)))
    if missing:
        raise ValueError('missing plan keys: %s' % sorted(missing))
    if plan['schema'] != PLAN_SCHEMA:
        raise ValueError('bad plan schema')
    h = _safe_str(plan['harness'], 'harness')
    if len(h) > 256 or not HARNESS_RE.match(h):
        raise ValueError('invalid harness name')
    m = _safe_str(plan['manifest_path'], 'manifest_path')
    if len(m) > 256 or not PATH_RE.match(m) or any(p in ('.', '..') for p in m.split('/')):
        raise ValueError('manifest_path must be a relative safe path')
    if m.split('/')[-1] != 'Cargo.toml':
        raise ValueError('manifest_path must end in Cargo.toml')
    feats = _str_list(plan['features'], 'features')
    for f in feats:
        if not isinstance(f, str) or not FEATURE_RE.match(f):
            raise ValueError('invalid feature name')
    if len(set(feats)) != len(feats):
        raise ValueError('duplicate features')
    if not isinstance(plan['tests'], bool):
        raise ValueError('tests must be bool')
    t = plan['timeout_secs']
    if not _is_int(t) or not 1 <= t <= 7200:
        raise ValueError('timeout_secs must be an integer in 1..7200')
    u = plan['unwind']
    if u is not None and (not _is_int(u) or not 1 <= u <= MAX_UNWIND):
        raise ValueError('unwind must be null or a positive integer')
    covers = _str_list(plan['required_covers'], 'required_covers')
    for c in covers:
        _safe_str(c, 'required cover')
    if len(set(covers)) != len(covers):
        raise ValueError('duplicate required_covers')
    outcome = plan['expected_outcome']
    if outcome == 'proven':
        if plan['expected_assertion'] is not None:
            raise ValueError('expected_assertion must be null for proven')
        if not covers:
            raise ValueError('proven plan requires nonempty required_covers')
    elif outcome == 'refuted':
        _safe_str(plan['expected_assertion'], 'expected_assertion')
    else:
        raise ValueError('expected_outcome must be proven or refuted')
    return plan


def build_command(plan, export_path):
    validate_plan(plan)
    if not isinstance(export_path, str) or not os.path.isabs(export_path) or '\x00' in export_path:
        raise ValueError('export_path must be an absolute path')
    argv = [CARGO_KANI, 'kani', '-Z', 'unstable-options', '--exact',
            '--harness', plan['harness'],
            '--export-json', export_path,
            '--manifest-path', plan['manifest_path'],
            '--output-format', 'regular',
            '--harness-timeout', '%ds' % plan['timeout_secs']]
    if plan['features']:
        argv += ['--features', ','.join(plan['features'])]
    if plan['tests']:
        argv.append('--tests')
    if plan['unwind'] is not None:
        argv += ['--unwind', str(plan['unwind'])]
    return argv


# ---------------------------------------------------------------- classifier

class _Verdict(Exception):
    def __init__(self, outcome, reason):
        Exception.__init__(self, reason)
        self.outcome = outcome
        self.reason = reason


def _fail(outcome, reason):
    raise _Verdict(outcome, reason)


def _result(plan, outcome, reason):
    harness = plan.get('harness') if isinstance(plan, dict) else None
    expected = plan.get('expected_outcome') if isinstance(plan, dict) else None
    return {'schema': RESULT_SCHEMA,
            'harness': harness if isinstance(harness, str) else '',
            'outcome': outcome,
            'accepted': bool(outcome == expected and outcome in ('proven', 'refuted')),
            'reason': reason or 'no reason'}


def _one(records, key, harness, what):
    if not isinstance(records, list):
        _fail('tool_error', '%s is not a list' % what)
    if len(records) != 1:
        _fail('tool_error', '%s must contain exactly one record, found %d' % (what, len(records)))
    rec = records[0]
    if not isinstance(rec, dict) or rec.get(key) != harness:
        _fail('tool_error', '%s record does not match harness' % what)
    return rec


COUNTER_STATUS = {'Success': 'passed', 'Failure': 'failed', 'Unreachable': 'unreachable',
                  'Undetermined': 'undetermined', 'Unknown': 'undetermined', 'Error': 'solver_error',
                  'Satisfied': 'satisfied', 'Unsatisfiable': 'unsatisfiable',
                  'Covered': 'covered', 'Uncovered': 'uncovered'}
COUNTER_KEYS = ('passed', 'failed', 'unreachable', 'undetermined', 'solver_error', 'satisfied',
                'unsatisfiable', 'covered', 'uncovered', 'total_properties')


def _classify(plan, document, execution):
    try:
        validate_plan(plan)
    except ValueError as e:
        _fail('tool_error', 'invalid plan: %s' % e)
    if not isinstance(execution, dict) or not _is_int(execution.get('returncode')) \
            or not isinstance(execution.get('timed_out'), bool) \
            or not isinstance(execution.get('oom_killed'), bool):
        _fail('tool_error', 'invalid execution record')
    rc = execution['returncode']
    if execution['timed_out']:
        _fail('timeout', 'outer timeout expired')
    if execution['oom_killed']:
        _fail('out_of_memory', 'outer node reported OOM kill')
    if rc < 0:
        _fail('tool_error', 'process terminated by signal %d without OOM evidence' % -rc)
    if not isinstance(document, dict):
        _fail('tool_error', 'export is not a JSON object')
    harness = plan['harness']
    meta = document.get('metadata')
    if not isinstance(meta, dict) or meta.get('version') != EXPORT_VERSION:
        _fail('tool_error', 'unsupported export metadata version')
    if meta.get('kani_version') != KANI_VERSION:
        _fail('tool_error', 'unexpected Kani version')
    hmeta = _one(document.get('harness_metadata'), 'pretty_name', harness, 'harness_metadata')
    vr = document.get('verification_results')
    if not isinstance(vr, dict):
        _fail('tool_error', 'verification_results missing')
    res = _one(vr.get('results'), 'harness_id', harness, 'results')
    errd = _one(document.get('error_details'), 'harness_id', harness, 'error_details')
    prop = _one(document.get('property_details'), 'harness_id', harness, 'property_details')
    summ = vr.get('summary')
    if not isinstance(summ, dict):
        _fail('tool_error', 'summary missing')
    for k in ('total_harnesses', 'executed', 'successful', 'failed'):
        if not _is_int(summ.get(k)):
            _fail('tool_error', 'summary.%s is not an integer' % k)
    if summ['total_harnesses'] != 1 or summ['executed'] != 1 \
            or summ['successful'] + summ['failed'] != 1:
        _fail('tool_error', 'summary counters inconsistent with one executed harness')
    rstatus = res.get('status')
    if rstatus not in ('Success', 'Failure'):
        _fail('tool_error', 'unknown result status')
    if (summ['successful'] == 1) != (rstatus == 'Success'):
        _fail('tool_error', 'summary disagrees with result status')
    if summ.get('status') != 'completed':
        _fail('inconclusive', 'summary status is %r, not completed' % (summ.get('status'),))
    has_errors = errd.get('has_errors')
    if not isinstance(has_errors, bool):
        _fail('tool_error', 'error_details.has_errors is not bool')
    exit_status = errd.get('exit_status')
    if has_errors and exit_status == 'timeout':
        _fail('timeout', 'CBMC reported timeout in error_details')
    if has_errors and exit_status == 'out_of_memory':
        _fail('out_of_memory', 'CBMC reported out of memory in error_details')
    attrs = hmeta.get('attributes')
    if not isinstance(attrs, dict) or not isinstance(attrs.get('should_panic'), bool):
        _fail('tool_error', 'harness attributes.should_panic missing')
    if attrs['should_panic']:
        _fail('inconclusive', 'should_panic harness cannot be proven or refuted here')
    checks = res.get('checks')
    if not isinstance(checks, list):
        _fail('tool_error', 'checks is not a list')
    seen_ids = set()
    counts = dict.fromkeys(COUNTER_KEYS, 0)
    for c in checks:
        if not isinstance(c, dict) or not isinstance(c.get('description'), str) \
                or not isinstance(c.get('category'), str):
            _fail('tool_error', 'malformed check')
        if c.get('status') not in COUNTER_STATUS:
            _fail('tool_error', 'unrecognized check status %r' % (c.get('status'),))
        cid = c.get('id')
        if not _is_int(cid) or cid in seen_ids:
            _fail('tool_error', 'missing or duplicate check id')
        seen_ids.add(cid)
        counts[COUNTER_STATUS[c['status']]] += 1
        counts['total_properties'] += 1
    pd = prop.get('property_details')
    if not isinstance(pd, dict):
        _fail('tool_error', 'property_details missing')
    for k in COUNTER_KEYS:
        if not _is_int(pd.get(k)):
            _fail('tool_error', 'property counter %s missing or not an integer' % k)
        if pd[k] != counts[k]:
            _fail('tool_error', 'property counter %s disagrees with checks' % k)
    if not checks:
        _fail('inconclusive', 'no checks were reported')
    if not any(c['category'] == 'assertion' for c in checks):
        _fail('inconclusive', 'no assertion check was reported')
    if counts['solver_error']:
        _fail('tool_error', 'solver error reported by property')
    noncover = [c for c in checks if c['category'] != 'cover']
    covers = [c for c in checks if c['category'] == 'cover']
    required = plan['required_covers']

    for desc in required:
        matches = [c for c in covers if c['description'] == desc]
        if len(matches) > 1:
            _fail('tool_error', 'required cover appears more than once: %s' % desc)
        if not matches:
            _fail('inconclusive', 'required cover missing: %s' % desc)
        if matches[0]['status'] != 'Satisfied':
            _fail('inconclusive', 'required cover not satisfied: %s' % desc)

    # consistency with the process exit code
    if rstatus == 'Success' and rc != 0:
        _fail('tool_error', 'Success report with nonzero exit code %d' % rc)
    if rstatus == 'Failure' and rc == 0:
        _fail('tool_error', 'Failure report with zero exit code')

    if any(c['status'] in ('Undetermined', 'Unknown') for c in noncover):
        _fail('inconclusive', 'undetermined property')
    fails = [c for c in checks if c['status'] == 'Failure']
    if rstatus == 'Failure' or fails:
        if rstatus != 'Failure' or not fails or not has_errors:
            _fail('tool_error', 'failure evidence inconsistent')
        if any(c['category'] == 'unwind' or c['description'].startswith('unwinding assertion')
               for c in fails):
            _fail('inconclusive', 'unwinding assertion failed')
        expected = plan['expected_assertion']
        if expected is None:
            _fail('inconclusive', 'failure observed but plan has no expected assertion')
        if errd.get('error_type') != 'assertion_failure' \
                or errd.get('failed_properties_type') != 'PanicsOnly' \
                or exit_status != 'properties_failed':
            _fail('inconclusive', 'failure is not an assertion/PanicsOnly/properties_failed counterexample')
        if any(c['category'] != 'assertion' or c['description'] != expected for c in fails):
            _fail('inconclusive', 'failing property is not the expected assertion')
        return 'refuted', 'expected assertion failed with a completed counterexample'

    # success path
    if has_errors:
        _fail('tool_error', 'has_errors set on a successful report')
    all_covered = bool(required)
    if not any(c['category'] == 'assertion' and c['status'] == 'Success' for c in checks):
        _fail('inconclusive', 'no reachable successful assertion')
    for c in noncover:
        if c['status'] == 'Success':
            continue
        if c['status'] == 'Unreachable' and all_covered:
            continue
        _fail('inconclusive', 'property %r has status %s' % (c['description'], c['status']))
    return 'proven', 'all properties verified and required covers satisfied'


def classify(plan, document, execution):
    try:
        outcome, reason = _classify(plan, document, execution)
    except _Verdict as v:
        outcome, reason = v.outcome, v.reason
    except Exception as e:  # fail closed
        outcome, reason = 'tool_error', 'classifier error: %s' % type(e).__name__
    return _result(plan, outcome, reason)


# ------------------------------------------------------------ input checking

def _pairs_hook(pairs):
    d = {}
    for k, v in pairs:
        if k in d:
            raise ValueError('duplicate JSON key %r' % k)
        d[k] = v
    return d


def _no_const(name):
    raise ValueError('invalid JSON constant %s' % name)


def parse_json_strict(data):
    if isinstance(data, bytes):
        data = data.decode('utf-8')
    return json.loads(data, object_pairs_hook=_pairs_hook, parse_constant=_no_const)


def check_environment(environ):
    # The immutable image and node provide these exact fixed values. They
    # cannot redirect a compiler or weaken a check; sanitized_env rebuilds
    # the child environment regardless. Other matching variables are refused.
    fixed = {'KANI_HOME': '/opt/kani', 'CARGO_TARGET_DIR': '/scratch/target',
             'RUSTFLAGS': '', 'CARGO_ENCODED_RUSTFLAGS': '', 'RUSTDOCFLAGS': '',
             'RUSTC_WRAPPER': '', 'RUSTC_WORKSPACE_WRAPPER': ''}
    bad = sorted(k for k, v in environ.items()
                 if UNSAFE_ENV.match(k) and (k not in fixed or v != fixed[k]))
    if bad:
        raise ValueError('unsafe environment variables: %s' % bad)


def _inside(root, path):
    return path == root or path.startswith(root.rstrip(os.sep) + os.sep)


def resolve_manifest(plan, workdir):
    root = os.path.realpath(workdir)
    path = os.path.realpath(os.path.join(root, plan['manifest_path']))
    if not _inside(root, path):
        raise ValueError('manifest resolves outside workdir')
    if not os.path.isfile(path):
        raise ValueError('manifest is not a regular file')
    return root, path


def _check_flags(table, where):
    if not isinstance(table, dict):
        raise ValueError('%s must be a table' % where)
    for key in table:
        norm = str(key).replace('_', '-')
        if norm in UNSAFE_KANI_FLAGS or norm.startswith('no-'):
            raise ValueError('unsafe Kani flag %r in %s' % (key, where))
    return True


def check_manifest_file(path):
    import tomllib
    with open(path, 'rb') as f:
        try:
            doc = tomllib.load(f)
        except Exception as e:
            raise ValueError('cannot parse %s: %s' % (path, e))
    for sect in ('package', 'workspace'):
        s = doc.get(sect)
        if not isinstance(s, dict):
            continue
        kani = (s.get('metadata') or {}).get('kani') if isinstance(s.get('metadata'), dict) else None
        if kani is None:
            continue
        if not isinstance(kani, dict):
            raise ValueError('%s.metadata.kani must be a table' % sect)
        if 'flags' in kani:
            _check_flags(kani['flags'], '%s.metadata.kani.flags' % sect)


def check_cargo_config(path):
    """Only Percolator's pinned stack-size setting is admitted, by parsed value."""
    import tomllib
    if os.path.islink(path) or not os.path.isfile(path):
        raise ValueError('unsafe cargo config path: %s' % path)
    with open(path, 'rb') as stream:
        try:
            doc = tomllib.load(stream)
        except Exception as exc:
            raise ValueError('cannot parse cargo config: %s' % exc)
    if doc != {'env': {'RUST_MIN_STACK': '8388608'}}:
        raise ValueError('cargo config is outside the fixed stack-size allowlist: %s' % path)


def scan_workdir(workdir, manifest, limit=20000):
    """Reject unsafe Cargo config and Kani manifest flags anywhere in workdir."""
    root = os.path.realpath(workdir)
    manifest = os.path.realpath(manifest)
    if not _inside(root, manifest):
        raise ValueError('manifest resolves outside workdir')
    # ancestors of the manifest inside workdir
    d = os.path.dirname(manifest)
    while True:
        for name in ('config', 'config.toml'):
            if os.path.lexists(os.path.join(d, '.cargo', name)):
                check_cargo_config(os.path.join(d, '.cargo', name))
        if d == root:
            break
        d = os.path.dirname(d)
    count = 0
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames[:] = [x for x in dirnames if x not in ('.git', 'target')]
        count += len(dirnames) + len(filenames)
        if count > limit:
            raise ValueError('workdir too large to scan')
        if os.path.basename(dirpath) == '.cargo':
            for name in filenames + dirnames:
                if name in ('config', 'config.toml'):
                    check_cargo_config(os.path.join(dirpath, name))
        for name in filenames:
            if name == 'Cargo.toml':
                full = os.path.join(dirpath, name)
                if os.path.islink(full):
                    raise ValueError('symlinked manifest: %s' % full)
                check_manifest_file(full)
    check_manifest_file(manifest)


# ---------------------------------------------------------------- execution

def sanitized_env(scratch, cargo_home=None):
    target = os.path.join(scratch, 'target')
    tmp = os.path.join(scratch, 'tmp')
    os.makedirs(target, exist_ok=True)
    os.makedirs(tmp, exist_ok=True)
    if cargo_home is None:
        cargo_home = '/deps/cargo' if os.path.isdir('/deps/cargo') else os.path.join(scratch, 'cargo')
    return {
        'PATH': '/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin',
        'HOME': '/tmp',
        'RUSTUP_HOME': '/usr/local/rustup',
        'KANI_HOME': '/opt/kani',
        'CARGO_HOME': cargo_home,
        'CARGO_TARGET_DIR': target,
        'TMPDIR': tmp,
        'CARGO_NET_OFFLINE': 'true',
        'CARGO_TERM_COLOR': 'never',
        'NO_COLOR': '1',
        'LANG': 'C.UTF-8',
    }


def _prepare_outdir(outdir):
    if os.path.islink(outdir) or not os.path.isdir(outdir):
        raise ValueError('outdir must be an existing real directory')
    for name in ARTIFACT_NAMES + ('result.json',):
        p = os.path.join(outdir, name)
        if os.path.lexists(p):
            if os.path.isdir(p) and not os.path.islink(p):
                raise ValueError('evidence path is a directory: %s' % name)
            os.unlink(p)


def _open_new(path):
    return os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644)


class _Budget:
    def __init__(self, limit):
        self.limit = limit
        self.used = 0
        self.overflow = False
        self.lock = threading.Lock()

    def take(self, n):
        with self.lock:
            if self.used + n > self.limit:
                self.overflow = True
                return False
            self.used += n
            return True


def _pump(fd, out_fd, budget, stop):
    try:
        while True:
            chunk = os.read(fd, 65536)
            if not chunk:
                return
            if not budget.take(len(chunk)):
                stop.set()
                return
            view = memoryview(chunk)
            while view:
                n = os.write(out_fd, view)
                view = view[n:]
    except OSError:
        pass


def _killpg(pgid):
    try:
        os.killpg(pgid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


def execute(argv, cwd, env, outdir, wall_timeout, max_bytes=MAX_EVIDENCE_BYTES):
    """Run argv in its own process group with bounded capture.

    Returns dict(returncode, timed_out, oom_killed, overflow)."""
    budget = _Budget(max_bytes)
    stop = threading.Event()
    out_fd = _open_new(os.path.join(outdir, 'stdout.log'))
    err_fd = _open_new(os.path.join(outdir, 'stderr.log'))
    try:
        proc = subprocess.Popen(argv, cwd=cwd, env=env, shell=False, stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                start_new_session=True, close_fds=True)
    except OSError as e:
        os.write(err_fd, ('failed to start: %s\n' % e).encode())
        os.close(out_fd)
        os.close(err_fd)
        return {'returncode': 127, 'timed_out': False, 'oom_killed': False, 'overflow': False}
    pgid = proc.pid
    threads = [threading.Thread(target=_pump, args=(proc.stdout.fileno(), out_fd, budget, stop), daemon=True),
               threading.Thread(target=_pump, args=(proc.stderr.fileno(), err_fd, budget, stop), daemon=True)]
    for t in threads:
        t.start()
    deadline = time.monotonic() + wall_timeout
    timed_out = False
    while True:
        if proc.poll() is not None:
            break
        if stop.is_set():
            break
        if time.monotonic() >= deadline:
            timed_out = True
            break
        time.sleep(0.02)
    if proc.poll() is None:
        _killpg(pgid)
    rc = proc.wait()
    _killpg(pgid)  # reap any descendants still in the group
    for t in threads:
        t.join(5)
    proc.stdout.close()
    proc.stderr.close()
    os.close(out_fd)
    os.close(err_fd)
    return {'returncode': rc, 'timed_out': timed_out, 'oom_killed': False,
            'overflow': budget.overflow, 'used': budget.used}


def _read_regular(path, limit):
    """Read a regular, non-symlink file without following links; None if absent."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    except FileNotFoundError:
        return None
    with os.fdopen(fd, 'rb') as f:
        st = os.fstat(f.fileno())
        if not stat.S_ISREG(st.st_mode):
            raise ValueError('not a regular file')
        if st.st_size > limit:
            raise OverflowError('too large')
        data = f.read(limit + 1)
        if len(data) > limit:
            raise OverflowError('too large')
        return data


def _hash_file(path):
    """Stream-hash the original bytes of a regular, non-symlink file."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    except FileNotFoundError:
        return None
    h = hashlib.sha256()
    total = 0
    with os.fdopen(fd, 'rb') as f:
        if not stat.S_ISREG(os.fstat(f.fileno()).st_mode):
            raise ValueError('not a regular file')
        for chunk in iter(lambda: f.read(1 << 20), b''):
            h.update(chunk)
            total += len(chunk)
    return {'bytes': total, 'sha256': h.hexdigest()}


def run_job(plan, workdir, outdir, scratch='/scratch', wall_timeout=None,
            max_bytes=MAX_EVIDENCE_BYTES, argv_fn=None, environ=None):
    """Run one job and return the result dict (also written to outdir/result.json)."""
    start = time.monotonic()
    argv = []
    execution = {'returncode': -1, 'timed_out': False, 'oom_killed': False}
    export = None
    overflow = False
    try:
        validate_plan(plan)
        check_environment(os.environ if environ is None else environ)
        _prepare_outdir(outdir)
        root, manifest = resolve_manifest(plan, workdir)
        scan_workdir(root, manifest)
        env = sanitized_env(scratch)
        outdir_abs = os.path.realpath(outdir)
        export_path = os.path.join(outdir_abs, EXPORT_NAME)
        argv = (argv_fn or build_command)(plan, export_path)
        if wall_timeout is None:
            wall_timeout = plan['timeout_secs'] + WALL_GRACE_SECS
        ex = execute(argv, root, env, outdir_abs, wall_timeout, max_bytes)
        overflow = ex['overflow']
        execution = {k: ex[k] for k in ('returncode', 'timed_out', 'oom_killed')}
        used = ex.get('used', 0)
        document = None
        if not overflow:
            try:
                export = _read_regular(export_path, max_bytes - used)
            except OverflowError:
                overflow = True
            except ValueError as e:
                raise _Verdict('tool_error', 'unsafe export file: %s' % e)
        if overflow:
            result = _result(plan, 'tool_error', 'raw evidence exceeded %d bytes' % max_bytes)
        else:
            try:
                document = parse_json_strict(export) if export is not None else None
            except (ValueError, RecursionError) as e:
                document = None
                if not (execution['timed_out'] or execution['oom_killed']):
                    result = _result(plan, 'tool_error', 'export is not valid JSON: %s' % e)
                    document = False
            if document is not False:
                result = classify(plan, document, execution)
    except _Verdict as v:
        result = _result(plan, v.outcome, v.reason)
    except (ValueError, OSError) as e:
        result = _result(plan, 'tool_error', 'rejected: %s' % e)
    # A rejected plan/source starts no subprocess and has empty output.
    # Preserve those empty streams so the signed evidence package remains
    # complete even for a pre-execution tool_error.
    for name in ('stdout.log', 'stderr.log'):
        path = os.path.join(outdir, name)
        if not os.path.lexists(path):
            try:
                os.close(_open_new(path))
            except OSError:
                pass
    artifacts = []
    for name in ARTIFACT_NAMES:
        try:
            entry = _hash_file(os.path.join(outdir, name))
        except (ValueError, OSError):
            entry = None
        if entry:
            artifacts.append(dict(entry, name=name))
    report = dict(result)
    report.update({
        'argv': argv,
        'tool': dict(TOOL_PIN),
        'duration_ms': int((time.monotonic() - start) * 1000),
        'exit_code': execution['returncode'],
        'timed_out': execution['timed_out'],
        'oom_killed': execution['oom_killed'],
        'evidence_overflow': overflow,
        'artifacts': artifacts,
    })
    try:
        fd = _open_new(os.path.join(outdir, 'result.json'))
        with os.fdopen(fd, 'w') as f:
            json.dump(report, f, sort_keys=True)
    except OSError as e:
        print('cannot write result.json: %s' % e, file=sys.stderr)
    return report


def main(argv=None):
    import argparse
    p = argparse.ArgumentParser(prog='runner.py')
    sub = p.add_subparsers(dest='cmd', required=True)
    r = sub.add_parser('run')
    r.add_argument('--plan', required=True)
    r.add_argument('--workdir', required=True)
    r.add_argument('--outdir', required=True)
    r.add_argument('--scratch', default='/scratch')
    args = p.parse_args(argv)
    try:
        with open(args.plan, 'rb') as f:
            plan = parse_json_strict(f.read(1 << 20))
        validate_plan(plan)
    except (OSError, ValueError) as e:
        print('invalid plan: %s' % e, file=sys.stderr)
        report = {'schema': RESULT_SCHEMA, 'harness': '', 'outcome': 'tool_error', 'accepted': False,
                  'reason': 'invalid plan: %s' % e, 'argv': [], 'tool': dict(TOOL_PIN), 'duration_ms': 0,
                  'exit_code': -1, 'timed_out': False, 'oom_killed': False, 'artifacts': []}
        sys.stdout.write(json.dumps(report, sort_keys=True) + '\n')
        return 2
    report = run_job(plan, args.workdir, args.outdir, scratch=args.scratch)
    print('outcome=%s accepted=%s: %s' % (report['outcome'], report['accepted'], report['reason']),
          file=sys.stderr)
    sys.stdout.write(json.dumps(report, sort_keys=True) + '\n')
    return 0 if report['accepted'] else 1


if __name__ == '__main__':
    sys.exit(main())
