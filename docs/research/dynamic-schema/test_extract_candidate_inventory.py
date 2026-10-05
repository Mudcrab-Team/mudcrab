"""Portable parser checks for the pinned-source inventory extractor."""
from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import extract_candidate_inventory as inventory


class PascalDeclarationTests(unittest.TestCase):
    def test_parse_xedit_ignores_comments_and_string_contents(self) -> None:
        source = """unit wbDefinitionsTES5;
procedure DefineTES5; begin
  wbRecord(ABCD, 'Active Record');
  wbRefRecord(EFGH, 'Linked Record');
  Text := 'wbRecord(''FAKE'', ''Not a declaration'')';
  // wbRecord('COMM', 'Commented Record');
  { ReferenceRecord(BRAC, 'Commented Helper'); }
  ReferenceRecord(TRP1, 'Trap One');
  ReferenceRecord(TRP2, 'Trap Two');
  ReferenceRecord(TRP3, 'Trap Three');
  ReferenceRecord(TRP4, 'Trap Four');
  ReferenceRecord(TRP5, 'Trap Five');
  ReferenceRecord(TRP6, 'Trap Six');
  ReferenceRecord(TRP7, 'Trap Seven');
  ReferenceRecord(TRP8, 'Trap Eight');
end;

end.
"""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            definition = root / inventory.XEDIT_DEF
            definition.parent.mkdir(parents=True)
            definition.write_text(source, encoding="utf-8")

            calls, helpers, first, last = inventory.parse_xedit(root)

        self.assertEqual(set(calls), {"ABCD", "EFGH", "TRP1", "TRP2", "TRP3", "TRP4", "TRP5", "TRP6", "TRP7", "TRP8"})
        self.assertEqual(calls["ABCD"][0]["name"], "Active Record")
        self.assertEqual(calls["EFGH"][0]["declaration"], "wbRefRecord")
        self.assertEqual(calls["TRP1"][0]["line"], 8)
        self.assertEqual(len(helpers), 8)
        self.assertEqual((first, last), (2, 16))


class MutagenDeclarationTests(unittest.TestCase):
    def test_parse_mutagen_counts_only_direct_major_record_objects(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            major = root / inventory.MUTAGEN_MAJOR_DIR
            major.mkdir(parents=True)
            (major / "Globals.xml").write_text(
                """<Root>
  <Object objType="Record" recordType="GMST" name="Global" />
  <Object objType="Record" recordType="GMST" name="FloatGlobal" />
  <Folder><Object objType="Record" recordType="FAKE" name="Nested" /></Folder>
</Root>
""",
                encoding="utf-8",
            )
            (root / inventory.MUTAGEN_MOD).write_text(
                """<Root>
  <Group name="Globals" refName="Global" />
  <GameReleaseOptions />
</Root>
""",
                encoding="utf-8",
            )
            header = root / "Mutagen.Bethesda.Skyrim/Records/SkyrimModHeader.xml"
            header.parent.mkdir(parents=True, exist_ok=True)
            header.write_text(
                """<Root>
  <Object objType="Record" recordType="TES4" name="Header" />
</Root>
""",
                encoding="utf-8",
            )

            records, count, groups, releases, header_records = inventory.parse_mutagen(root)

        self.assertEqual(count, 2)
        self.assertEqual(len(records["GMST"]), 2)
        self.assertNotIn("FAKE", records)
        self.assertEqual(groups["Global"][0]["group_name"], "Globals")
        self.assertEqual(len(releases), 1)
        self.assertEqual(header_records["TES4"][0]["object_name"], "Header")


if __name__ == "__main__":
    unittest.main()
