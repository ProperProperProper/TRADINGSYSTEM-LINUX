import tempfile
import unittest
from pathlib import Path

from papercompmoomoo.deploy import code_hash


class CodeHash(unittest.TestCase):
    def test_it_changes_with_any_source_byte_and_ignores_caches_and_other_files(self):
        project = Path(tempfile.mkdtemp())
        package = project / "src" / "papercompmoomoo"
        (package / "__pycache__").mkdir(parents=True)
        (package / "a.py").write_text("x = 1")
        base = code_hash(project)
        (package / "__pycache__" / "a.cpython.py").write_text("ignored")
        (package / "notes.txt").write_text("ignored")
        self.assertEqual(code_hash(project), base)
        (package / "a.py").write_text("x = 2")
        self.assertNotEqual(code_hash(project), base)


if __name__ == "__main__":
    unittest.main()
