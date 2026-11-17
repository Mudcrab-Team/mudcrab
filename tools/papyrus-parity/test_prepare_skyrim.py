from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from prepare_skyrim import prepare


class PreparationTests(unittest.TestCase):
    def inputs(self, root):
        game, pex, template = (root / name for name in ("game", "pex", "template"))
        (game / "Data").mkdir(parents=True)
        (game / "SkyrimSE.exe").write_bytes(b"probe executable")
        (game / "Skyrim_Default.ini").write_text("[General]\n")
        pex.mkdir()
        for name in ("Grand", "Parent", "Inheritance", "NestedEnd", "NestedBegin", "Latent"):
            (pex / f"MCParity{name}.pex").write_bytes(b"probe bytecode")
        template.mkdir()
        return game, pex, template

    def test_output_inside_template_is_rejected_before_copying(self):
        with tempfile.TemporaryDirectory() as directory:
            game, pex, template = self.inputs(Path(directory))
            output = template / "session"
            with patch("prepare_skyrim.shutil.copytree", side_effect=RuntimeError("copy reached")):
                with self.assertRaisesRegex(ValueError, "inside.*prefix"):
                    prepare(game, pex, template, output)
            self.assertFalse(output.exists())

    def test_copied_profile_file_links_do_not_overwrite_external_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            game, pex, template = self.inputs(root)
            external = root / "external.ini"
            external.write_text("original")
            paths = ("Documents/My Games/Skyrim Special Edition/Skyrim.ini",
                     "Documents/My Games/Skyrim Special Edition/SkyrimPrefs.ini",
                     "AppData/Local/Skyrim Special Edition/plugins.txt",
                     "AppData/Local/Skyrim Special Edition/loadorder.txt")
            for name in paths:
                link = template / "pfx/drive_c/users/steamuser" / name
                link.parent.mkdir(parents=True, exist_ok=True)
                link.symlink_to(external)
            output = root / "session"
            prepare(game, pex, template, output)
            self.assertEqual(external.read_text(), "original")
            for name in paths:
                file = output / "compatdata/pfx/drive_c/users/steamuser" / name
                self.assertTrue(file.is_file())
                self.assertFalse(file.is_symlink())


if __name__ == "__main__":
    unittest.main()
