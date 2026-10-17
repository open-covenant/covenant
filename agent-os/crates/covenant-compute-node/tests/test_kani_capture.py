"""Local bootstrap capture-shim safety checks; no Docker or Kani invocation."""
import ast
import os
from pathlib import Path
import tempfile
import types
import unittest

SOURCE = Path(__file__).resolve().parents[1] / 'src' / 'kani_capture.py'
tree = ast.parse(SOURCE.read_text())
# Load only imports and read_file; never execute the container entry point.
module = types.ModuleType('capture_helpers')
selected = [node for node in tree.body if isinstance(node, (ast.Import, ast.ImportFrom, ast.FunctionDef))]
exec(compile(ast.Module(body=selected, type_ignores=[]), str(SOURCE), 'exec'), module.__dict__)

class CaptureFiles(unittest.TestCase):
    def test_preserves_original_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            file = Path(directory) / 'stdout.log'
            original = b'\x00raw\n\xffoutput\r\n'
            file.write_bytes(original)
            self.assertEqual(module.read_file(file, len(original)), original)
    def test_rejects_symlinks_hardlinks_and_oversized_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'source'
            source.write_bytes(b'full log')
            link = root / 'link'
            link.symlink_to(source)
            with self.assertRaises(ValueError):
                module.read_file(link, 100)
            hard = root / 'hard'
            os.link(source, hard)
            with self.assertRaises(ValueError):
                module.read_file(hard, 100)
            hard.unlink()
            with self.assertRaises(ValueError):
                module.read_file(source, 2)
    def test_rejects_directories(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ValueError):
                module.read_file(Path(directory), 100)

if __name__ == '__main__':
    unittest.main()
