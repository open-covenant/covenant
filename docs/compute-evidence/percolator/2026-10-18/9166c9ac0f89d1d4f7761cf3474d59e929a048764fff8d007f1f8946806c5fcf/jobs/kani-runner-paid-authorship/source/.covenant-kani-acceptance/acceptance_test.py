"""Buyer-authored acceptance tests; local preparation, not product implementation."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import unittest
ROOT = Path(os.environ.get('COVENANT_KANI_REPO_ROOT', '.')).resolve()
HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('covenant_kani_runner', ROOT/'deploy/kani-check/runner.py')
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)
CASES = json.loads((HERE/'classifier-cases.json').read_text())
PLAN = json.loads((HERE/'example-plan.json').read_text())
class KaniContract(unittest.TestCase):
    def test_classifier_cases(self):
        for case in CASES:
            with self.subTest(case=case['name']):
                result = runner.classify(case['plan'], case['document'], case['execution'])
                self.assertIsInstance(result, dict)
                self.assertEqual(result['outcome'], case['expected'])
                self.assertIsInstance(result['reason'], str)
                self.assertTrue(result['reason'])
                self.assertEqual(result['harness'], case['plan']['harness'])
                self.assertEqual(result['schema'], 'covenant.kani.result.v1')
                self.assertIs(type(result['accepted']), bool)
                self.assertEqual(result['accepted'], result['outcome'] == case['plan']['expected_outcome'])
    def test_plan_validation(self):
        runner.validate_plan(PLAN)
        bads = [dict(PLAN, extra_arguments=['--no-unwinding-checks']), dict(PLAN, harness=''), dict(PLAN, harness='--only-codegen'), dict(PLAN, manifest_path='/etc/passwd'), dict(PLAN, manifest_path='../Cargo.toml'), dict(PLAN, features=['x; touch /tmp/pwned']), dict(PLAN, timeout_secs=0), dict(PLAN, timeout_secs=True), dict(PLAN, unwind=0), dict(PLAN, tests='yes'), dict(PLAN, expected_outcome='refuted', expected_assertion=None)]
        for plan in bads:
            with self.subTest(plan=plan):
                with self.assertRaises(ValueError):
                    runner.validate_plan(plan)
    def test_bounded_fixed_command(self):
        argv = runner.build_command(PLAN, '/evidence/kani-export.json')
        self.assertIsInstance(argv, list)
        self.assertTrue(all(isinstance(a,str) for a in argv))
        self.assertEqual(argv[0], '/usr/local/cargo/bin/cargo-kani')
        self.assertEqual(argv[1], 'kani')
        self.assertIn('--exact', argv)
        self.assertEqual(argv[argv.index('--harness')+1], PLAN['harness'])
        self.assertEqual(argv[argv.index('--export-json')+1], '/evidence/kani-export.json')
        self.assertIn('-Z', argv)
        self.assertIn('unstable-options', argv)
        for forbidden in ['sh','-c','--only-codegen','--no-unwinding-checks','--no-assertion-reach-checks','--no-default-checks','--prove-safety-only','--cbmc-args','--test']:
            self.assertNotIn(forbidden, argv)
    def test_malformed_document(self):
        for raw in [None, {}, [], {'verification_results':None}]:
            with self.subTest(raw=raw):
                result=runner.classify(PLAN,raw,{'returncode':0,'timed_out':False,'oom_killed':False})
                self.assertEqual(result['outcome'],'tool_error')
                self.assertFalse(result['accepted'])
if __name__ == '__main__':
    unittest.main()
