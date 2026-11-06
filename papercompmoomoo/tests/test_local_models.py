import os
import tempfile
import time
import unittest
from pathlib import Path

from papercompmoomoo import local_models as lm


class Tidy(unittest.TestCase):
    def make(self, root, name, size=100, age=10 * 3600, incomplete=False):
        d = Path(root) / name
        d.mkdir(parents=True)
        f = d / "model.safetensors"
        f.write_bytes(b"x" * size)
        if incomplete:
            (d / "x.incomplete").write_bytes(b"y")
        old = time.time() - age
        for p in (f, d):
            os.utime(p, (old, old))
        return d

    def test_only_the_models_in_use_stay_and_a_download_in_progress_is_never_touched(self):
        with tempfile.TemporaryDirectory() as root:
            self.make(root, "active")
            self.make(root, "old-model", 500)
            self.make(root, "half-downloaded", 50, incomplete=True)
            self.make(root, "just-started", 10, age=60)
            would = lm.tidy(Path(root), {"active"}, apply=False)
            self.assertEqual([r["model"] for r in would], ["old-model"])
            self.assertTrue((Path(root) / "old-model").exists(), "a dry run removes nothing")
            done = lm.tidy(Path(root), {"active"}, apply=True)
            self.assertEqual((done[0]["model"], done[0]["bytes"], done[0]["removed"]), ("old-model", 500, True))
            self.assertEqual(sorted(p.name for p in Path(root).iterdir()), ["active", "half-downloaded", "just-started"])

    def test_exactly_one_model_is_kept_and_a_second_complete_model_is_removed_at_once(self):
        with tempfile.TemporaryDirectory() as root:
            self.make(root, "active", 600)
            self.make(root, "the-other-complete-model", 700)
            self.assertEqual(lm.keep_list("active"), {"active"})
            lm.tidy(Path(root), lm.keep_list("active"), apply=True)
            self.assertEqual([p.name for p in Path(root).iterdir()], ["active"], "only ever one model locally")
            self.assertEqual(lm.tidy(Path(root) / "no-such-dir", {"a"}), [])

    def test_the_prompt_cache_is_kept_under_its_limit_oldest_first(self):
        with tempfile.TemporaryDirectory() as root:
            for i, age in enumerate((300, 200, 100)):
                f = Path(root) / f"{i}.safetensors"
                f.write_bytes(b"x" * 400)
                os.utime(f, (time.time() - age, time.time() - age))
            self.assertEqual(lm.trim_cache(Path(root), 800, apply=False), 400)
            self.assertEqual(len(list(Path(root).iterdir())), 3, "a dry run removes nothing")
            self.assertEqual(lm.trim_cache(Path(root), 800, apply=True), 400)
            self.assertEqual(sorted(p.name for p in Path(root).iterdir()), ["1.safetensors", "2.safetensors"], "the oldest went")


if __name__ == "__main__":
    unittest.main()
