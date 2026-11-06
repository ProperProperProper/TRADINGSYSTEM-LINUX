import os
import tempfile
import unittest
from pathlib import Path

from papercompmoomoo import runtime


class RuntimeDirectory(unittest.TestCase):
    def test_every_entry_point_can_restore_the_project_directory(self):
        old = Path.cwd()
        with tempfile.TemporaryDirectory() as other:
            os.chdir(other)
            try:
                self.assertEqual(runtime.enter_project(), runtime.PROJECT_ROOT)
                self.assertEqual(Path.cwd(), runtime.PROJECT_ROOT)
                self.assertTrue((Path.cwd() / "src" / "papercompmoomoo").is_dir())
                self.assertTrue((Path.cwd() / "tests").is_dir())
            finally:
                os.chdir(old)


if __name__ == "__main__":
    unittest.main()
