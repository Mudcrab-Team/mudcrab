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

    def test_stale_script_cannot_replace_required_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            game, pex, template = self.inputs(root)
            (pex / "MCParityGrand.pex").rename(pex / "MCParityStale.pex")
            with self.assertRaisesRegex(ValueError, "MCParityGrand.pex"):
                prepare(game, pex, template, root / "session")
            self.assertFalse((root / "session").exists())

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

    def test_escaping_user_ancestry_is_rejected_before_unlinking(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            game, pex, template = self.inputs(root)
            external = root / "external-user"
            external.mkdir()
            real_documents = root / "external-documents"
            real_documents.mkdir()
            documents = external / "Documents"
            documents.symlink_to(real_documents)
            user = template / "pfx/drive_c/users/steamuser"
            user.parent.mkdir(parents=True)
            user.symlink_to(external)
            with self.assertRaisesRegex(ValueError, "escaped.*prefix"):
                prepare(game, pex, template, root / "session")
            self.assertTrue(documents.is_symlink())
            self.assertEqual(list(external.iterdir()), [documents])
            self.assertEqual(list(real_documents.iterdir()), [])

    def test_nested_profile_directories_are_checked_before_creation(self):
        for relative in ("Documents/My Games", "Documents/My Games/Skyrim Special Edition/Logs",
                         "AppData"):
            with self.subTest(relative=relative), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                game, pex, template = self.inputs(root)
                external = root / "external-directory"
                external.mkdir()
                link = template / "pfx/drive_c/users/steamuser" / relative
                link.parent.mkdir(parents=True)
                link.symlink_to(external)
                with self.assertRaisesRegex(ValueError, "escaped.*prefix"):
                    prepare(game, pex, template, root / "session")
                self.assertEqual(list(external.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
