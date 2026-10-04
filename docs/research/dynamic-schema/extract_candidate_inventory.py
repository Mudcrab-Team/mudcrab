#!/usr/bin/env python3
"""Reproduce the narrow provisional Skyrim RECORD source inventory.

This reads declarations only. It does not evaluate Pascal, build/run either tool,
read retail plugins, or claim a current/latest retail corpus. Pins are deliberate.
"""
from __future__ import annotations

import argparse
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from xml.parsers import expat
import json
import re
import subprocess
import sys

XEDIT_PIN = "9fb016884bec138ea6c7b872cec831537d464c3e"
MUTAGEN_PIN = "4f533562ee0c70347d47c1979d5464d42b06ee6b"
XEDIT_DEF = "Core/wbDefinitionsTES5.pas"
MUTAGEN_MAJOR_DIR = "Mutagen.Bethesda.Skyrim/Records/Major Records"
MUTAGEN_MOD = "Mutagen.Bethesda.Skyrim/Records/SkyrimMod.xml"


def git(root: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def verify_checkout(root: Path, expected_pin: str, label: str) -> dict:
    root = root.resolve()
    top = git(root, "rev-parse", "--show-toplevel")
    if Path(top).resolve() != root:
        raise ValueError(f"{label} path is not the Git worktree root: {root}")
    head = git(root, "rev-parse", "HEAD")
    if head != expected_pin:
        raise ValueError(f"{label} pin mismatch: expected {expected_pin}, found {head}")
    if git(root, "status", "--porcelain"):
        raise ValueError(f"{label} checkout is dirty: {root}")
    branch = git(root, "branch", "--show-current")
    upstream_proc = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
        text=True, capture_output=True,
    )
    upstream = upstream_proc.stdout.strip() if upstream_proc.returncode == 0 else None
    ahead_behind = None
    if upstream:
        pair = git(root, "rev-list", "--left-right", "--count", f"HEAD...{upstream}").split()
        ahead_behind = {"ahead": int(pair[0]), "behind": int(pair[1])}
    return {
        "checkout": str(root), "pin": head, "branch": branch,
        "upstream": upstream, "ahead_behind_vs_upstream": ahead_behind,
        "working_tree": "clean",
    }


def strip_pascal_comments(source: str) -> str:
    """Blank Pascal comments while retaining strings and original line offsets."""
    out = list(source)
    i, n, state, depth = 0, len(source), "normal", 0
    while i < n:
        char = source[i]
        nxt = source[i + 1] if i + 1 < n else ""
        if state == "normal":
            if char == "'":
                state = "string"
                i += 1
            elif char == "{":
                state, depth = "brace", 1
                out[i] = " "
                i += 1
            elif char == "(" and nxt == "*":
                state, depth = "paren", 1
                out[i] = out[i + 1] = " "
                i += 2
            elif char == "/" and nxt == "/":
                state = "line"
                out[i] = out[i + 1] = " "
                i += 2
            else:
                i += 1
        elif state == "string":
            if char == "'" and nxt == "'":
                i += 2
            elif char == "'":
                state = "normal"
                i += 1
            else:
                i += 1
        elif state == "brace":
            if char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
                if depth == 0:
                    state = "normal"
            if char != "\n":
                out[i] = " "
            i += 1
        elif state == "paren":
            if char == "(" and nxt == "*":
                depth += 1
                out[i] = out[i + 1] = " "
                i += 2
            elif char == "*" and nxt == ")":
                depth -= 1
                out[i] = out[i + 1] = " "
                i += 2
                if depth == 0:
                    state = "normal"
            else:
                if char != "\n":
                    out[i] = " "
                i += 1
        else:
            if char == "\n":
                state = "normal"
            else:
                out[i] = " "
            i += 1
    return "".join(out)


def line_number(source: str, offset: int) -> int:
    return source.count("\n", 0, offset) + 1


def parse_xedit(root: Path) -> tuple[dict, list[tuple[str, str, int]], int, int]:
    path = root / XEDIT_DEF
    source = path.read_text(encoding="utf-8-sig")
    active = strip_pascal_comments(source)
    start = re.search(r"(?m)^procedure DefineTES5;\s*begin\b", active)
    if not start:
        raise ValueError("Could not find DefineTES5 implementation start")
    end = active.find("\nend;\n\nend.", start.start())
    if end < 0:
        raise ValueError("Could not find DefineTES5 implementation end")
    procedure = active[start.start():end]
    calls: dict[str, list[dict]] = defaultdict(list)
    record_call = re.compile(
        r"\b(wbRefRecord|wbRecord)\s*\(\s*([A-Z0-9_]{4})\s*,\s*'((?:[^']|'')*)'", re.S
    )
    for match in record_call.finditer(procedure):
        sig = match.group(2)
        absolute = start.start() + match.start()
        calls[sig].append({
            "path": XEDIT_DEF, "line": line_number(source, absolute),
            "declaration": match.group(1), "name": match.group(3).replace("''", "'"),
            "evidence_method": "active Pascal call signature argument; comments stripped, scoped to DefineTES5",
        })
    helper = []
    helper_call = re.compile(r"\bReferenceRecord\s*\(\s*([A-Z0-9_]{4})\s*,\s*'((?:[^']|'')*)'")
    for match in helper_call.finditer(procedure):
        sig = match.group(1)
        absolute = start.start() + match.start()
        name = match.group(2).replace("''", "'")
        line = line_number(source, absolute)
        calls[sig].append({
            "path": XEDIT_DEF, "line": line, "declaration": "ReferenceRecord", "name": name,
            "evidence_method": "active typed helper call; ReferenceRecord body delegates to wbRefRecord",
        })
        helper.append((sig, name, line))
    procedure_start_line = line_number(source, start.start())
    procedure_end_line = line_number(source, end + len("\nend;"))
    if len(helper) != 8:
        raise ValueError(f"Expected eight active ReferenceRecord helper calls at this pin; found {len(helper)}")
    return dict(calls), helper, procedure_start_line, procedure_end_line


def parse_xml_objects(path: Path, predicate) -> list[dict]:
    found = []
    parser = expat.ParserCreate()
    depth = [0]

    def start(name, attrs):
        current = depth[0]
        if current == 1 and name == "Object" and predicate(attrs):
            found.append((parser.CurrentLineNumber, attrs.copy()))
        depth[0] += 1

    def end(_name):
        depth[0] -= 1

    parser.StartElementHandler = start
    parser.EndElementHandler = end
    parser.Parse(path.read_bytes(), True)
    return [{"line": line, **attrs} for line, attrs in found]


def parse_mutagen(root: Path) -> tuple[dict, int, dict, list[dict], dict]:
    base = root / MUTAGEN_MAJOR_DIR
    by_sig: dict[str, list[dict]] = defaultdict(list)
    for path in sorted(base.glob("*.xml")):
        objects = parse_xml_objects(
            path,
            lambda a: a.get("objType") == "Record" and re.fullmatch(r"[A-Z0-9_]{4}", a.get("recordType", "")) is not None,
        )
        for obj in objects:
            sig = obj["recordType"]
            by_sig[sig].append({
                "path": path.relative_to(root).as_posix(), "line": obj["line"],
                "object_name": obj.get("name"), "abstract": obj.get("abstract") == "true",
                "major_flag": obj.get("majorFlag") == "true",
                "evidence_method": "direct child Object in Major Records XML with objType=Record and 4-character recordType",
            })
    mod_path = root / MUTAGEN_MOD
    group_refs: dict[str, list[dict]] = defaultdict(list)
    releases = []
    parser = expat.ParserCreate()
    depth = [0]

    def mod_start(name, attrs):
        current = depth[0]
        if name == "Group" and attrs.get("refName"):
            group_refs[attrs["refName"]].append({
                "path": MUTAGEN_MOD, "line": parser.CurrentLineNumber, "group_name": attrs.get("name"),
            })
        if name == "GameReleaseOptions":
            releases.append({"path": MUTAGEN_MOD, "line": parser.CurrentLineNumber})
        depth[0] += 1

    parser.StartElementHandler = mod_start
    parser.EndElementHandler = lambda _name: depth.__setitem__(0, depth[0] - 1)
    parser.Parse(mod_path.read_bytes(), True)
    for declarations in by_sig.values():
        for declaration in declarations:
            declaration["model_group_registration"] = group_refs.get(declaration["object_name"], [])

    header_path = root / "Mutagen.Bethesda.Skyrim/Records/SkyrimModHeader.xml"
    header = parse_xml_objects(
        header_path,
        lambda a: a.get("objType") == "Record" and a.get("recordType") == "TES4",
    )
    header_decl = []
    for obj in header:
        header_decl.append({
            "path": header_path.relative_to(root).as_posix(), "line": obj["line"],
            "object_name": obj.get("name"), "abstract": obj.get("abstract") == "true",
            "major_flag": obj.get("majorFlag") == "true",
            "evidence_method": "separate mod-header XML root Object declares recordType TES4; excluded from Major Records XML scrape",
        })
    return dict(by_sig), sum(map(len, by_sig.values())), dict(group_refs), releases, {"TES4": header_decl}


def build_inventory(xroot: Path, mroot: Path) -> dict:
    xroot, mroot = xroot.resolve(), mroot.resolve()
    xstatus = verify_checkout(xroot, XEDIT_PIN, "xEdit")
    mstatus = verify_checkout(mroot, MUTAGEN_PIN, "Mutagen")
    xedit, helper, proc_start, proc_end = parse_xedit(xroot)
    mutagen, mutagen_object_count, _groups, releases, header = parse_mutagen(mroot)

    x_sigs, m_sigs = set(xedit), set(mutagen)
    plugin_x = x_sigs - {"TES4"}
    entries = []
    for sig in sorted(x_sigs | m_sigs):
        xd = xedit.get(sig, [])
        md = mutagen.get(sig, [])
        if sig == "TES4":
            md = header.get("TES4", [])
            relation = "header_only_excluded_from_plugin_major_record_set"
            caveats = ["File header signature, not a Skyrim plugin major RECORD candidate for catalog count."]
        elif xd and md:
            relation, caveats = "declared_by_both_sources", []
        elif xd:
            relation = "xedit_only_candidate"
            caveats = ["Source asymmetry is provisional; absence from Mutagen major-record XML is not proof that the game never emits this signature."]
        else:
            relation = "mutagen_only_candidate"
            caveats = ["Source asymmetry is provisional; inspect registration, generation, and retail evidence before interpreting as a game-record omission."]
        if sig in ("VOLI", "LENS"):
            caveats += [
                "xEdit declaration is inside if wbIsSkyrimSE; that predicate also includes Skyrim VR and Enderal SE.",
                "Mutagen model registers this type without a per-release gate; SkyrimMod supports LE, SE, SE GOG, VR, Enderal LE, Enderal SE, and Enderal SE GOG.",
            ]
        if sig in ("CLDC", "PWAT", "SCPT"):
            caveats.append("xEdit labels it among unused records with empty GRUP in skyrim.esm; this is source commentary, not retail corpus validation.")
        if sig == "HAIR":
            caveats.append("xEdit labels it unused in Skyrim but contained in Skyrim.esm; retained as a declared signature.")
        if sig == "PLYR":
            caveats.append("xEdit labels it Player Reference; Mutagen has no corresponding Major Records XML declaration at this pin. Do not treat the asymmetry as a bug without game evidence.")
        if sig in ("GMST", "GLOB") and len(md) > 1:
            caveats.append("Mutagen models several value-specialized CLR objects under the same RECORD signature; these are duplicate source declarations for one wire signature.")
        if sig in ("RGDL", "SCOL"):
            caveats.append("xEdit group-order comment says unused in Skyrim but contained in Skyrim.esm; this is source commentary, not current retail validation.")
        entries.append({
            "signature": sig, "xedit_declarations": xd, "mutagen_declarations": md,
            "reconciliation": relation,
            "duplicate_declaration_counts": {"xedit": len(xd), "mutagen": len(md)},
            "caveats": caveats,
        })

    all_x_decls = sum(map(len, xedit.values()))
    shared = sum(1 for entry in entries if entry["signature"] != "TES4" and entry["xedit_declarations"] and entry["mutagen_declarations"])
    summary = {
        "xedit_active_record_declaration_calls_in_DefineTES5": all_x_decls,
        "xedit_unique_declared_signatures_including_TES4_header": len(x_sigs),
        "xedit_unique_plugin_candidate_signatures_excluding_TES4_header": len(plugin_x),
        "mutagen_direct_major_record_xml_declaration_objects": mutagen_object_count,
        "mutagen_unique_major_record_signatures": len(m_sigs),
        "shared_plugin_candidate_signatures": shared,
        "xedit_only_plugin_candidate_signatures": sorted(plugin_x - m_sigs),
        "mutagen_only_plugin_candidate_signatures": sorted(m_sigs - plugin_x),
        "xedit_only_declaration_duplicates": {sig: len(decls) for sig, decls in sorted(xedit.items()) if len(decls) > 1},
        "mutagen_duplicate_signature_declarations": {sig: len(decls) for sig, decls in sorted(mutagen.items()) if len(decls) > 1},
        "xedit_header_signature_excluded": ["TES4"],
    }
    # Retain reviewed line anchors as facts, not copied source text.
    xanchors = [
        {"path": "Core/wbInterface.pas", "line": 5338, "fact": "wbIsSkyrimSE evaluates true for gmTES5VR, gmSSE, and gmEnderalSE."},
        {"path": XEDIT_DEF, "line": 10206, "fact": "if wbIsSkyrimSE then begin encloses VOLI and LENS declarations at lines 10207 and 10221."},
        {"path": XEDIT_DEF, "line": 10883, "fact": "LENS and VOLI group-order registration is also gated by wbIsSkyrimSE."},
        {"path": XEDIT_DEF, "line": 10116, "fact": "TES4 is declared as Main File Header; retained separately from plugin record signatures."},
        {"path": XEDIT_DEF, "line": 10714, "fact": "Following xEdit comment labels CLDC, HAIR, PWAT, and SCPT as unused/empty-group cases; HAIR note says contained in Skyrim.esm."},
        {"path": "Core/wbDefinitionsCommon.pas", "line": 6510, "fact": "IsSSE chooses its first alternative whenever wbIsSkyrimSE is true; this is used for conditional fields and names as well as definitions."},
        {"path": "Core/wbDefinitionsCommon.pas", "line": 6326, "fact": "IsVR chooses first alternative for gmTES5VR or gmFO4VR."},
        {"path": "Core/wbDefinitionsTES5Saves.pas", "line": 6233, "fact": "DefineTES5Saves initializes TES5 save parsing, calls DefineTES5, then save-specific DefineTES5SavesA and DefineTES5SavesS."},
        {"path": "Core/wbInterface.pas", "line": 4584, "fact": "xEdit distinguishes tsPlugins and tsSaves tool sources."},
        {"path": XEDIT_DEF, "line": 2134, "fact": "ReferenceRecord typed helper delegates to wbRefRecord; its eight invocations at lines 4184-4191 declare placed-trap signatures."},
    ]
    for sig, name, line in helper:
        xanchors.append({"path": XEDIT_DEF, "line": line, "fact": f"ReferenceRecord call declares {sig} ({name})."})
    xanchors.append({"path": "Core/wbInterface.pas", "line": 5333, "fact": "wbIsSkyrim covers gmTES5, gmEnderal, gmTES5VR, gmSSE, and gmEnderalSE source modes."})
    manchors = [
        {"path": MUTAGEN_MOD, "line": releases[0]["line"], "fact": "SkyrimMod lists release options SkyrimLE, SkyrimSE, SkyrimSEGog, SkyrimVR, EnderalLE, EnderalSE, EnderalSEGog."},
        {"path": "Mutagen.Bethesda.Skyrim/Records/Major Records/LensFlare.xml", "line": 3, "fact": "LensFlare model declares recordType LENS; no per-release gate in declaration."},
        {"path": "Mutagen.Bethesda.Skyrim/Records/Major Records/VolumetricLighting.xml", "line": 3, "fact": "VolumetricLighting model declares recordType VOLI; no per-release gate in declaration."},
        {"path": "Mutagen.Bethesda.Skyrim/Records/SkyrimMajorRecord.xml", "line": 3, "fact": "Common major-record base model contains FormVersion and Version2; field versioning is distinct from per-release record applicability."},
    ]
    return {
        "artifact_kind": "provisional_newest_SE_record_source_candidate_inventory",
        "artifact_status": "P0_source_evidence_only_not_accepted_corpus",
        "generated_at_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "scope": "Candidate inventory of declared four-character Skyrim plugin major RECORD signatures from pinned xEdit DefineTES5 and Mutagen Skyrim model XML. It does not assert semantic completeness, current retail coverage, or newest official game build identity.",
        "sources": {
            "xedit": {
                **xstatus,
                "declaration_source": {
                    "path": XEDIT_DEF, "procedure_start_line": proc_start, "procedure_end_line": proc_end,
                    "method_note": "Active wbRecord/wbRefRecord calls plus ReferenceRecord(SIG, name) helper invocations inside DefineTES5; the helper body delegates to wbRefRecord.",
                },
            },
            "mutagen": {
                **mstatus,
                "declaration_source": {"directory": MUTAGEN_MAJOR_DIR, "method_note": "Direct child Object declarations in each Major Records XML file."},
                "model_release_scope_source": {"path": MUTAGEN_MOD, "release_option_lines": [releases[0]["line"], releases[-1]["line"]]},
            },
        },
        "extraction_methods": {
            "xedit": "Read Core/wbDefinitionsTES5.pas at pinned commit; strip Pascal brace, (* *) and // comments while preserving strings/newlines; scope to implementation procedure DefineTES5; extract active wbRecord/wbRefRecord calls and typed ReferenceRecord(SIG, name) calls. The ReferenceRecord helper body at line 2134 delegates to wbRefRecord. Keep declarations by source line; classify direct wbIsSkyrimSE enclosure manually from source anchors. This is declaration scraping, not evaluation of the full Pascal program.",
            "mutagen": "Parse XML files directly under Mutagen.Bethesda.Skyrim/Records/Major Records; take only root-level child Object elements with objType=\"Record\" and a four-character recordType. Preserve all declarations so multiple CLR objects sharing one wire signature remain visible. Group registration is joined by object name from SkyrimMod.xml. The shared SkyrimMod release list is not applied as a per-record version gate. Record classes nested in SkyrimMod cell/list-group models are not inferred by a direct top-level Group refName join; an empty join does not mean unregistered.",
            "comparison": "Compare exact 4-character signatures after excluding xEdit TES4 file header from plugin major-record set; retain TES4 row as separately classified evidence, with its Mutagen SkyrimModHeader declaration shown separately. Differences are flagged as source asymmetries, not assumed omissions.",
        },
        "summary": summary,
        "source_gate_anchors": {"xedit": xanchors, "mutagen": manchors},
        "entries": entries,
        "interpretive_caveats": [
            "xEdit wbIsSkyrimSE includes gmTES5VR and gmEnderalSE as well as gmSSE; its name does not make its gated declarations exclusive to retail Special Edition.",
            "xEdit IsSSE(...) is also used to choose version-specific fields inside records. Those field branches must not be counted as separate record types.",
            "Mutagen SkyrimMod advertises a shared schema for LE, SE, SE GOG, VR, and Enderal variants; model inclusion alone does not establish that a record exists or is valid in every release.",
            "xEdit save parsing has a separate DefineTES5Saves entrypoint and save-specific routines; save chapters are outside this plugin RECORD inventory even though the save initializer reuses DefineTES5.",
            "xEdit source comments identify some signatures as unused or empty-group in skyrim.esm; this is a tool-source clue, not a check against a pinned retail plugin corpus.",
            "The complete official newest-SE retail Data/Creation manifest is not pinned in this artifact. There was no plugin read, binary/runtime validation, build, test, xEdit execution, Mutagen execution, or upstream script execution.",
            "The Mutagen group join records only direct SkyrimMod Group entries. Placed references and cells also appear through nested cell/list-group model declarations; an empty direct join is not an omission signal.",
            "The parent reports an executable pin for Skyrim SE/AE 1.7.104.0 (Steam build 24914197, exe SHA256 in validation_scope_and_provenance); this source inventory does not establish that the complete Data/Creation manifest is pinned or that the build is newest.",
            "The parent reports a separate importer-native 1.6.1170 reference and an existing Mutagen 0.54.4 / .NET 9.0.318 oracle with placed-dump F0005 evidence. Those are distinct evidence lanes and were not inspected or rerun here.",
        ],
        "validation_scope_and_provenance": {
            "this_kickoff_performed": False,
            "this_kickoff_performed_note": "This artifact records source declarations only. It did not read a Skyrim.esm, execute the oracle, validate a package, run xEdit/Mutagen, or confirm a retail catalog.",
            "source_reference_pins": {"xedit": XEDIT_PIN, "mutagen": MUTAGEN_PIN},
            "separate_executable_pin_reported_by_parent": {
                "status": "parent-reported repository lock evidence; not inspected in this kickoff",
                "lock_file": "/home/dev/.t3/projects/mudcrab-reverse-engineering/tools.lock.toml",
                "game": "Skyrim SE/AE 1.7.104.0", "steam_build": "24914197",
                "exe_sha256": "846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f",
                "scope_limit": "An executable/build pin does not pin the complete Data/Creation content corpus or establish that this is the newest released corpus.",
            },
            "separate_importer_and_oracle_context_reported_by_parent": {
                "static_importer_native_evidence": "1.6.1170; source/validation meaning is importer-native only, not a current retail executable pin.",
                "oracle_source": "/home/dev/.t3/projects/mudcrab-reverse-engineering/oracles/records/Program.cs",
                "oracle_dependencies": {"Mutagen.Bethesda.Skyrim": "0.54.4", ".NET SDK": "9.0.318"},
                "reported_evidence": "Placed dump only; F0005 reports seven count categories matched on the same Skyrim.esm.",
                "provenance_note": "These details were supplied by the parent and were not independently inspected or rerun during this source-only kickoff.",
            },
            "remaining_p0_prerequisite": "Full Data/Creation manifest is not pinned (RE T18 pending). Pin and identify the official data/build corpus before treating this candidate inventory as an accepted newest-SE record catalog.",
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--xedit-root", required=True, type=Path, help="Pinned xEdit source checkout")
    parser.add_argument("--mutagen-root", required=True, type=Path, help="Pinned Mutagen source checkout")
    parser.add_argument("--output", required=True, type=Path, help="Destination JSON file")
    args = parser.parse_args()
    try:
        inventory = build_inventory(args.xedit_root, args.mutagen_root)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(inventory, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    except (OSError, ValueError, subprocess.CalledProcessError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    print(json.dumps({"output": str(args.output.resolve()), "summary": inventory["summary"]}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
