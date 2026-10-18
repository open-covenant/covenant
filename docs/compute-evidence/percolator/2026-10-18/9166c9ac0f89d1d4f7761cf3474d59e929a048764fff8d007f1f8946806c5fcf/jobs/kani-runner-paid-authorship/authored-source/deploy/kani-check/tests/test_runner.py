import copy
import hashlib
import importlib.util
import json
import os
import shutil
import stat
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('kani_runner_under_test', HERE.parent / 'runner.py')
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

PLAN = {
    'schema': 'covenant.kani.plan.v1', 'harness': 'proofs::route_proof', 'manifest_path': 'Cargo.toml',
    'tests': False, 'features': ['fuzz'], 'timeout_secs': 60, 'unwind': None,
    'expected_outcome': 'proven', 'expected_assertion': None,
    'required_covers': ['cover condition: route reachable'],
}
OK = {'returncode': 0, 'timed_out': False, 'oom_killed': False}
H = PLAN['harness']


def chk(i, status, desc, cat):
    return {'id': i, 'function': H, 'status': status, 'description': desc, 'category': cat,
            'location': {'file': 'a.rs', 'line': '1', 'column': '1'}}


def doc(checks=None, status='Success', **over):
    checks = checks if checks is not None else [
        chk(1, 'Success', 'assertion failed: x', 'assertion'),
        chk(2, 'Satisfied', 'cover condition: route reachable', 'cover')]
    counts = {k: 0 for k in runner.COUNTER_KEYS}
    for c in checks:
        counts[runner.COUNTER_STATUS[c['status']]] += 1
        counts['total_properties'] += 1
    ok = status == 'Success'
    d = {'metadata': {'version': '1.0', 'kani_version': '0.68.0', 'extra': 1},
         'harness_metadata': [{'pretty_name': H, 'attributes': {'should_panic': False}}],
         'verification_results': {'summary': {'total_harnesses': 1, 'executed': 1, 'status': 'completed',
                                              'successful': int(ok), 'failed': int(not ok)},
                                  'results': [{'harness_id': H, 'status': status, 'checks': checks}]},
         'error_details': [{'harness_id': H, 'has_errors': not ok}],
         'property_details': [{'harness_id': H, 'property_details': counts}]}
    d.update(over)
    return d


class Classify(unittest.TestCase):
    def test_proven(self):
        r = runner.classify(PLAN, doc(), OK)
        self.assertEqual((r['outcome'], r['accepted']), ('proven', True))

    def test_outer_flags_override(self):
        self.assertEqual(runner.classify(PLAN, doc(), dict(OK, timed_out=True))['outcome'], 'timeout')
        self.assertEqual(runner.classify(PLAN, doc(), dict(OK, oom_killed=True))['outcome'], 'out_of_memory')
        self.assertEqual(runner.classify(PLAN, doc(), dict(OK, returncode=-9))['outcome'], 'tool_error')
        self.assertEqual(runner.classify(PLAN, doc(), dict(OK, returncode=137))['outcome'], 'tool_error')

    def test_bad_execution(self):
        for ex in [None, {}, dict(OK, returncode=True), dict(OK, timed_out=0)]:
            self.assertEqual(runner.classify(PLAN, doc(), ex)['outcome'], 'tool_error')

    def test_invalid_plan_fails_closed(self):
        r = runner.classify({}, doc(), OK)
        self.assertEqual((r['outcome'], r['accepted']), ('tool_error', False))

    def refuted_plan(self):
        return dict(PLAN, expected_outcome='refuted', expected_assertion='assertion failed: x')

    def refuted_doc(self, desc='assertion failed: x', cat='assertion', etype='assertion_failure'):
        d = doc([chk(1, 'Failure', desc, cat)], status='Failure')
        d['error_details'][0].update(error_type=etype, failed_properties_type='PanicsOnly',
                                     exit_status='properties_failed')
        return d

    def test_refuted(self):
        ex = dict(OK, returncode=1)
        r = runner.classify(self.refuted_plan(), self.refuted_doc(), ex)
        self.assertEqual((r['outcome'], r['accepted']), ('refuted', True))
        self.assertEqual(runner.classify(self.refuted_plan(), self.refuted_doc('other'), ex)['outcome'],
                         'inconclusive')
        # exit code zero with a failure report is inconsistent
        self.assertEqual(runner.classify(self.refuted_plan(), self.refuted_doc(), OK)['outcome'], 'tool_error')
        # a proven plan is never accepted on a counterexample
        self.assertFalse(runner.classify(PLAN, self.refuted_doc(), ex)['accepted'])

    def test_refuted_plan_with_proof_not_accepted(self):
        r = runner.classify(self.refuted_plan(), doc(), OK)
        self.assertEqual((r['outcome'], r['accepted']), ('proven', False))

    def test_nonzero_exit_with_success_doc(self):
        self.assertEqual(runner.classify(PLAN, doc(), dict(OK, returncode=1))['outcome'], 'tool_error')

    def test_duplicate_and_mismatched_records(self):
        d = doc()
        d['property_details'].append(copy.deepcopy(d['property_details'][0]))
        self.assertEqual(runner.classify(PLAN, d, OK)['outcome'], 'tool_error')
        d = doc()
        d['harness_metadata'][0]['pretty_name'] = 'other'
        self.assertEqual(runner.classify(PLAN, d, OK)['outcome'], 'tool_error')
        d = doc()
        d['error_details'] = []
        self.assertEqual(runner.classify(PLAN, d, OK)['outcome'], 'tool_error')

    def test_counter_inconsistency_and_null(self):
        d = doc()
        d['property_details'][0]['property_details']['passed'] = 5
        self.assertEqual(runner.classify(PLAN, d, OK)['outcome'], 'tool_error')
        d = doc()
        d['property_details'][0]['property_details']['total_properties'] = None
        self.assertEqual(runner.classify(PLAN, d, OK)['outcome'], 'tool_error')

    def test_unknown_status_is_tool_error(self):
        d = doc()
        d['verification_results']['results'][0]['checks'][0]['status'] = 'Weird'
        self.assertEqual(runner.classify(PLAN, d, OK)['outcome'], 'tool_error')

    def test_duplicate_required_cover(self):
        checks = [chk(1, 'Success', 'a', 'assertion'), chk(2, 'Satisfied', 'cover condition: route reachable', 'cover'),
                  chk(3, 'Satisfied', 'cover condition: route reachable', 'cover')]
        self.assertEqual(runner.classify(PLAN, doc(checks), OK)['outcome'], 'tool_error')

    def test_unrequired_cover_may_be_unsatisfiable(self):
        checks = [chk(1, 'Success', 'a', 'assertion'), chk(2, 'Satisfied', 'cover condition: route reachable', 'cover'),
                  chk(3, 'Unsatisfiable', 'other cover', 'cover')]
        self.assertEqual(runner.classify(PLAN, doc(checks), OK)['outcome'], 'proven')

    def test_no_assertion_is_inconclusive(self):
        checks = [chk(1, 'Success', 'u', 'unwind'), chk(2, 'Satisfied', 'cover condition: route reachable', 'cover')]
        self.assertEqual(runner.classify(PLAN, doc(checks), OK)['outcome'], 'inconclusive')

    def test_unreachable_other_property_needs_covers(self):
        checks = [chk(1, 'Success', 'a', 'assertion'), chk(2, 'Unreachable', 'b', 'assertion'),
                  chk(3, 'Satisfied', 'cover condition: route reachable', 'cover')]
        self.assertEqual(runner.classify(PLAN, doc(checks), OK)['outcome'], 'proven')
        rp = dict(PLAN, expected_outcome='refuted', expected_assertion='z', required_covers=[])
        self.assertEqual(runner.classify(rp, doc(checks), OK)['outcome'], 'inconclusive')

    def test_summary_partial(self):
        d = doc()
        d['verification_results']['summary']['status'] = 'completed_with_fail_fast'
        self.assertEqual(runner.classify(PLAN, d, OK)['outcome'], 'inconclusive')

    def test_malformed_documents(self):
        for raw in [None, 3, [], {}, {'metadata': {}}, {'verification_results': None}]:
            self.assertEqual(runner.classify(PLAN, raw, OK)['outcome'], 'tool_error')

    def test_solver_error_tool_error(self):
        checks = [chk(1, 'Error', 'a', 'assertion'), chk(2, 'Satisfied', 'cover condition: route reachable', 'cover')]
        self.assertEqual(runner.classify(PLAN, doc(checks), OK)['outcome'], 'tool_error')


class PlanAndCommand(unittest.TestCase):
    def bad(self, **kw):
        with self.assertRaises(ValueError):
            runner.validate_plan(dict(PLAN, **kw))

    def test_rejections(self):
        self.bad(extra=1)
        self.bad(features=['a,b'])
        self.bad(features='x')
        self.bad(manifest_path='a/../Cargo.toml')
        self.bad(manifest_path='a\\Cargo.toml')
        self.bad(manifest_path='Cargo.lock')
        self.bad(unwind=True)
        self.bad(timeout_secs=7201)
        self.bad(required_covers=[])
        self.bad(required_covers=[''])
        self.bad(required_covers=['x'] * 100)
        self.bad(expected_assertion='x')
        self.bad(harness='a' * 300)
        self.bad(schema='other')
        p = dict(PLAN)
        del p['features']
        with self.assertRaises(ValueError):
            runner.validate_plan(p)
        with self.assertRaises(ValueError):
            runner.validate_plan(dict(PLAN, expected_outcome='refuted', expected_assertion=''))

    def test_command_exact(self):
        argv = runner.build_command(PLAN, '/e/kani-export.json')
        self.assertEqual(argv, ['/usr/local/cargo/bin/cargo-kani', 'kani', '-Z', 'unstable-options', '--exact',
                                '--harness', H, '--export-json', '/e/kani-export.json', '--manifest-path',
                                'Cargo.toml', '--output-format', 'regular', '--harness-timeout', '60s',
                                '--features', 'fuzz'])
        argv = runner.build_command(dict(PLAN, tests=True, unwind=5, features=['a', 'b']), '/e/x.json')
        self.assertIn('--tests', argv)
        self.assertEqual(argv[argv.index('--unwind') + 1], '5')
        self.assertEqual(argv[argv.index('--features') + 1], 'a,b')
        self.assertNotIn('--jobs', argv)
        with self.assertRaises(ValueError):
            runner.build_command(PLAN, 'relative.json')


class Fs(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.work = os.path.join(self.tmp, 'work')
        self.out = os.path.join(self.tmp, 'out')
        self.scratch = os.path.join(self.tmp, 'scratch')
        for d in (self.work, self.out, self.scratch):
            os.mkdir(d)
        Path(self.work, 'Cargo.toml').write_text('[package]\nname="x"\nversion="0.1.0"\n')

    def stub(self, code):
        def fn(plan, export_path):
            return [sys.executable, '-c', code, export_path]
        return fn

    def run_job(self, code, plan=PLAN, **kw):
        return runner.run_job(plan, self.work, self.out, scratch=self.scratch, argv_fn=self.stub(code),
                              environ={}, **kw)


class Execution(Fs):
    def test_success_hashes_and_artifacts(self):
        d = json.dumps(doc())
        code = 'import sys;print("log text {\\"outcome\\":\\"proven\\"}");open(sys.argv[1],"w").write(%r)' % d
        rep = self.run_job(code)
        self.assertEqual(rep['outcome'], 'proven')
        self.assertTrue(rep['accepted'])
        names = [a['name'] for a in rep['artifacts']]
        self.assertEqual(names, ['stdout.log', 'stderr.log', 'kani-export.json'])
        for a in rep['artifacts']:
            data = Path(self.out, a['name']).read_bytes()
            self.assertEqual(a['bytes'], len(data))
            self.assertEqual(a['sha256'], hashlib.sha256(data).hexdigest())
        self.assertEqual(json.loads(Path(self.out, 'result.json').read_text())['outcome'], 'proven')

    def test_offline_environment(self):
        code = ('import os,sys,json;e=dict(os.environ);open(sys.argv[1],"w").write("{}");'
                'print(json.dumps(e));print(os.getcwd())')
        rep = self.run_job(code)
        out = Path(self.out, 'stdout.log').read_text().splitlines()
        env = json.loads(out[0])
        self.assertEqual(env['CARGO_NET_OFFLINE'], 'true')
        self.assertEqual(env['CARGO_TERM_COLOR'], 'never')
        self.assertEqual(env['PATH'], '/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin')
        self.assertEqual(env['CARGO_TARGET_DIR'], os.path.join(self.scratch, 'target'))
        self.assertEqual(env['TMPDIR'], os.path.join(self.scratch, 'tmp'))
        self.assertEqual(env['KANI_HOME'], '/opt/kani')
        self.assertEqual(env['HOME'], '/tmp')
        self.assertTrue(os.path.isdir(env['TMPDIR']) and os.path.isdir(env['CARGO_TARGET_DIR']))
        self.assertEqual(out[1], os.path.realpath(self.work))
        self.assertEqual(rep['outcome'], 'tool_error')

    def test_timeout_kills_group(self):
        code = ('import subprocess,sys,time;'
                'subprocess.Popen([sys.executable,"-c","import time;time.sleep(60)"]);time.sleep(60)')
        rep = self.run_job(code, wall_timeout=0.5)
        self.assertEqual(rep['outcome'], 'timeout')
        self.assertTrue(rep['timed_out'])
        self.assertFalse(rep['accepted'])

    def test_overflow_is_tool_error(self):
        code = 'import sys;sys.stdout.write("x"*5000000)'
        rep = self.run_job(code, max_bytes=100000)
        self.assertEqual(rep['outcome'], 'tool_error')
        self.assertTrue(rep['evidence_overflow'])
        self.assertLessEqual(os.path.getsize(os.path.join(self.out, 'stdout.log')), 100000)

    def test_export_overflow(self):
        code = 'import sys;open(sys.argv[1],"w").write("x"*5000)'
        rep = self.run_job(code, max_bytes=1000)
        self.assertEqual(rep['outcome'], 'tool_error')
        self.assertTrue(rep['evidence_overflow'])

    def test_stale_export_not_reused(self):
        Path(self.out, 'kani-export.json').write_text(json.dumps(doc()))
        rep = self.run_job('pass')
        self.assertEqual(rep['outcome'], 'tool_error')
        self.assertNotIn('kani-export.json', [a['name'] for a in rep['artifacts']])

    def test_export_symlink_rejected(self):
        target = os.path.join(self.tmp, 'target.json')
        Path(target).write_text(json.dumps(doc()))
        code = 'import os,sys;os.symlink(%r,sys.argv[1])' % target
        rep = self.run_job(code)
        self.assertEqual(rep['outcome'], 'tool_error')
        self.assertFalse(rep['accepted'])

    def test_invalid_json_and_duplicate_keys(self):
        rep = self.run_job('import sys;open(sys.argv[1],"w").write(\'{"a":1,"a":2}\')')
        self.assertEqual(rep['outcome'], 'tool_error')
        with self.assertRaises(ValueError):
            runner.parse_json_strict('{"a":1,"a":2}')
        with self.assertRaises(ValueError):
            runner.parse_json_strict('{"a":NaN}')

    def test_missing_executable(self):
        rep = runner.run_job(PLAN, self.work, self.out, scratch=self.scratch, environ={},
                             argv_fn=lambda p, e: ['/nonexistent/cargo-kani'])
        self.assertEqual(rep['outcome'], 'tool_error')

    def test_nonzero_exit_without_export(self):
        rep = self.run_job('import sys;sys.exit(3)')
        self.assertEqual((rep['outcome'], rep['exit_code']), ('tool_error', 3))


class Rejections(Fs):
    def reject(self):
        rep = self.run_job('open(__import__("sys").argv[1],"w").write("{}")')
        self.assertEqual(rep['outcome'], 'tool_error')
        self.assertIn('rejected', rep['reason'])
        self.assertFalse(Path(self.out, 'stdout.log').exists())

    def test_cargo_config(self):
        os.mkdir(os.path.join(self.work, '.cargo'))
        Path(self.work, '.cargo', 'config.toml').write_text('[build]\nrustc-wrapper="x"\n')
        self.reject()

    def test_unsafe_flags(self):
        for sect in ('package', 'workspace'):
            for flag in ('no-unwinding-checks', 'only-codegen', 'cbmc-args', 'prove_safety_only'):
                Path(self.work, 'Cargo.toml').write_text(
                    '[%s]\n[%s.metadata.kani.flags]\n%s = true\n' % (sect, sect, flag))
                with self.subTest(sect=sect, flag=flag):
                    self.reject()

    def test_manifest_symlink_escape(self):
        os.unlink(os.path.join(self.work, 'Cargo.toml'))
        outside = os.path.join(self.tmp, 'Cargo.toml')
        Path(outside).write_text('[package]\nname="x"\n')
        os.symlink(outside, os.path.join(self.work, 'Cargo.toml'))
        self.reject()

    def test_unsafe_env(self):
        rep = runner.run_job(PLAN, self.work, self.out, scratch=self.scratch,
                             environ={'RUSTFLAGS': '-C x'}, argv_fn=self.stub('pass'))
        self.assertEqual(rep['outcome'], 'tool_error')
        self.assertIn('RUSTFLAGS', rep['reason'])

    def test_safe_flags_allowed(self):
        Path(self.work, 'Cargo.toml').write_text(
            '[package]\nname="x"\n[package.metadata.kani.flags]\ndefault-unwind = 3\n')
        runner.scan_workdir(self.work, os.path.join(os.path.realpath(self.work), 'Cargo.toml'))


class Cli(Fs):
    def test_plan_with_duplicate_keys_rejected(self):
        p = os.path.join(self.tmp, 'plan.json')
        Path(p).write_text('{"schema":"covenant.kani.plan.v1","schema":"x"}')
        import contextlib
        import io
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(io.StringIO()):
            rc = runner.main(['run', '--plan', p, '--workdir', self.work, '--outdir', self.out])
        self.assertNotEqual(rc, 0)
        self.assertEqual(json.loads(buf.getvalue())['outcome'], 'tool_error')


if __name__ == '__main__':
    unittest.main()
