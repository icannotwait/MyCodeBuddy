import importlib.util
import pathlib
import unittest
from unittest import mock
import tempfile
import json
import hashlib

SPEC = importlib.util.spec_from_file_location('shards', pathlib.Path(__file__).with_name('rust-test-shards.py'))
shards = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(shards)

class InventoryTests(unittest.TestCase):
    def test_new_attempt_invalidates_previous_success_before_audit(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = pathlib.Path(temporary)
            (output/'summary.json').write_text('{"success":true,"complete_core_suite_passed":true}')
            with mock.patch.object(shards.sys, 'argv', ['shards','--output',temporary]), mock.patch.object(shards, 'audit_source', side_effect=ValueError('source changed')):
                with self.assertRaises(ValueError):
                    shards.main()
            summary = json.loads((output/'summary.json').read_text())
            self.assertFalse(summary['success'])
            self.assertFalse(summary['complete_core_suite_passed'])
            self.assertEqual(summary['status'], 'running')
        shards.ACTIVE_RESULT.clear()

    def test_extra_gate_in_test_free_source_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            name = 'src-tauri/src/product.rs'
            source = 'fn product() {}\n'
            path = root/name
            path.parent.mkdir(parents=True)
            path.write_text('#[cfg(any(not(codeg_test_shard), codeg_test_shard = "0"))]\n'+source)
            manifest = {'source_files':{name:hashlib.sha256(source.encode()).hexdigest()}, 'files':[], 'gates':[], 'shards':list(range(8))}
            with self.assertRaises(ValueError):
                shards.audit_source(manifest, root)

    def test_moving_gate_from_test_to_production_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            name = 'src-tauri/src/product.rs'
            source = 'fn product() {}\n#[test]\nfn test_product() {}\n'
            path = root/name
            path.parent.mkdir(parents=True)
            path.write_text('#[cfg(any(not(codeg_test_shard), codeg_test_shard = "0"))]\n'+source)
            manifest = {'source_files':{name:hashlib.sha256(source.encode()).hexdigest()},
                        'files':[{'file':name,'gates':1,'shard':0}],
                        'gates':[{'file':name,'line':2,'column':0}], 'shards':list(range(8))}
            with self.assertRaises(ValueError):
                shards.audit_source(manifest, root)

    def test_low_memory_alias_runs_complete_wrapper(self):
        package = json.loads((shards.ROOT/'package.json').read_text())
        self.assertEqual(package['scripts'].get('rust:test:sharded:low-memory'), 'python3 scripts/rust-test-shards.py')

    def test_build_script_reads_the_generated_shard_count(self):
        source = (shards.ROOT/'src-tauri/build.rs').read_text()
        self.assertIn('include_str!("test-shard-count.txt")', source)
        self.assertIn('0..count', source)

    def test_mid_run_build_input_changes_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            (root/'Cargo.lock').write_text('changed')
            summary = {'build_inputs_sha256':{'Cargo.lock':hashlib.sha256(b'original').hexdigest()}, 'manifest_sha256':'unused'}
            with mock.patch.object(shards, 'audit_source'):
                with self.assertRaises(ValueError):
                    shards.audit_run_inputs({}, summary, root)

    def test_mid_run_manifest_changes_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            (root/'scripts').mkdir()
            (root/'scripts/rust-test-shards-manifest.json').write_text('changed')
            summary = {'build_inputs_sha256':{}, 'manifest_sha256':hashlib.sha256(b'original').hexdigest()}
            with mock.patch.object(shards, 'audit_source'):
                with self.assertRaises(ValueError):
                    shards.audit_run_inputs({}, summary, root)

    def test_nested_cfg_and_features(self):
        cfg = {'test', 'unix', 'target_os="linux"', 'feature="test-utils"'}
        self.assertTrue(shards.eval_cfg('cfg (all (test, any(unix, windows), not(feature = "tauri-runtime")))', cfg))
        self.assertFalse(shards.eval_cfg('cfg (all(test, feature = "tauri-runtime"))', cfg))
        self.assertTrue(shards.eval_cfg('cfg (target_os = "linux")', cfg))

    def test_unknown_or_malformed_cfg_fails_closed(self):
        for text in ['cfg (maybe(test))', 'cfg(not(test,unix))', 'cfg(test) garbage']:
            with self.assertRaises(ValueError):
                shards.eval_cfg(text, {'test'})

    def test_names_not_counts_are_audited(self):
        with self.assertRaises(ValueError):
            shards.audit_names(['a::one', 'b::two'], ['a::one', 'b::wrong'])

    def test_duplicates_are_rejected(self):
        with self.assertRaises(ValueError):
            shards.audit_names(['a::one'], ['a::one', 'a::one'])

    def test_ignored_tests_remain_in_listing(self):
        self.assertEqual(shards.parse_listing('a::one: test\nb::ignored: test\n\n2 tests, 0 benchmarks\n'), ['a::one','b::ignored'])

    def test_unknown_listing_type_fails_closed(self):
        with self.assertRaises(ValueError):
            shards.parse_listing('a::one: benchmark\n')

    def test_execution_accounting_preserves_ignored_tests(self):
        result = shards.parse_execution('test result: ok. 4 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s', 5)
        self.assertEqual(result['ignored'], 1)

    def test_filtered_or_unaccounted_execution_rejected(self):
        for log in ['test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.01s',
                    'test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s']:
            with self.assertRaises(ValueError):
                shards.parse_execution(log, 5)

    def test_stripping_only_generated_gates_preserves_indentation(self):
        source = 'mod tests {\n    #[cfg(any(not(codeg_test_shard), codeg_test_shard = "2"))]\n    #[test]\n    fn foo() {}\n}\n'
        self.assertEqual(shards.strip_gates(source), 'mod tests {\n    #[test]\n    fn foo() {}\n}\n')

if __name__ == '__main__':
    unittest.main()
