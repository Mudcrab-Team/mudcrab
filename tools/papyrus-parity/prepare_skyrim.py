#!/usr/bin/env python3
"""Create an isolated, disposable Skyrim profile containing only the probe plugin."""
import argparse
import configparser
import hashlib
import json
from pathlib import Path
import shutil
import struct

CASES = ("MCParityInheritance", "MCParityNestedEnd", "MCParityNestedBegin", "MCParityLatent")


def subrecord(name, data):
    return name.encode("ascii") + struct.pack("<H", len(data)) + data


def record(name, form_id, data):
    return struct.pack("<4sIIIIHH", name.encode("ascii"), len(data), 0, form_id, 0, 44, 0) + data


def plugin():
    header = subrecord("HEDR", struct.pack("<fII", 1.7, len(CASES) + 1, 0x800 + len(CASES)))
    header += subrecord("CNAM", b"Mudcrab parity probes\0")
    header += subrecord("MAST", b"Skyrim.esm\0") + subrecord("DATA", bytes(8))
    quests = bytearray()
    for index, name in enumerate(CASES):
        encoded = name.encode("ascii")
        vmad = struct.pack("<HHH", 5, 2, 1) + struct.pack("<H", len(encoded)) + encoded
        vmad += struct.pack("<BH", 0, 0)  # script status and property count
        vmad += struct.pack("<BHHH", 2, 0, 0, 0)  # version, fragments, filename length, aliases
        data = subrecord("EDID", encoded + b"\0") + subrecord("VMAD", vmad)
        data += subrecord("DNAM", struct.pack("<HBBII", 1, 50, 0, 0, 0))
        data += subrecord("NEXT", b"") + subrecord("ANAM", struct.pack("<I", 0))
        quests += record("QUST", 0x01000800 + index, data)
    group = struct.pack("<4sI4siHHI", b"GRUP", len(quests) + 24, b"QUST", 0, 0, 0, 0)
    return record("TES4", 0, header) + group + quests


def require_contained(path, prefix):
    if not path.resolve().is_relative_to(prefix.resolve()):
        raise ValueError(f"Directory escaped the copied prefix: {path}")


def prepare(game, pex, prefix_template, output):
    game = game.resolve(strict=True)
    pex = pex.resolve(strict=True)
    prefix_template = prefix_template.resolve(strict=True)
    output = output.resolve()
    if output.is_relative_to(prefix_template):
        raise ValueError("Output inside the prefix template is not allowed")
    if output.exists():
        raise ValueError("Use a fresh output directory; an existing game/profile is never modified")
    scripts = [pex / f"{name}.pex" for name in (*CASES, "MCParityParent", "MCParityGrand")]
    for script in scripts:
        if not script.is_file():
            raise ValueError(f"Missing required PEX fixture: {script.name}")
    output.mkdir(parents=True, mode=0o700)
    target = output / "game"
    data = target / "Data"
    (data / "Scripts").mkdir(parents=True)
    for name in ("SkyrimSE.exe", "steam_api64.dll", "bink2w64.dll"):
        (target / name).symlink_to(game / name)
    for item in (game / "Data").iterdir():
        if item.is_file() and (item.suffix.lower() == ".bsa" or item.name in
                              ("Skyrim.esm", "Update.esm", "Dawnguard.esm", "HearthFires.esm", "Dragonborn.esm")):
            (data / item.name).symlink_to(item)
    for item in scripts:
        shutil.copy2(item, data / "Scripts" / item.name)
    (data / "MCParity.esp").write_bytes(plugin())
    (target / "steam_appid.txt").write_text("489830\n")
    (target / "MCParityStart.txt").write_text("coc QASmoke\n")
    compat = output / "compatdata"
    shutil.copytree(prefix_template, compat, symlinks=True)
    user = compat / "pfx/drive_c/users/steamuser"
    require_contained(user, compat)
    # Wine may create user-directory links into the real home. Replace those
    # only in the copied prefix before any game process can write through them.
    for name in ("Documents", "My Documents", "Desktop", "Downloads", "Pictures", "Music", "Videos"):
        folder = user / name
        if folder.is_symlink():
            folder.unlink()
        folder.mkdir(exist_ok=True)
    documents = user / "Documents/My Games/Skyrim Special Edition"
    require_contained(documents, compat)
    documents.mkdir(parents=True, exist_ok=True)
    require_contained(documents / "Logs/Script", compat)
    (documents / "Logs/Script").mkdir(parents=True, exist_ok=True)
    for name in ("Skyrim.ini", "SkyrimPrefs.ini"):
        filename = documents / name
        if filename.is_symlink():
            filename.unlink()
    ini = configparser.ConfigParser(strict=False)
    ini.optionxform = str
    ini.read(game / "Skyrim_Default.ini")
    for section in ("General", "Papyrus", "Archive"):
        if section not in ini:
            ini[section] = {}
    ini["General"]["sStartingConsoleCommand"] = ""
    ini["General"]["sIntroSequence"] = ""
    ini["Papyrus"].update({"bEnableLogging": "1", "bEnableTrace": "1", "bLoadDebugInformation": "1"})
    ini["Archive"].update({"bInvalidateOlderFiles": "1", "sResourceDataDirsFinal": ""})
    with (documents / "Skyrim.ini").open("w") as stream:
        ini.write(stream)
    prefs = """[Display]
bFull Screen=0
bBorderless=0
iSize W=960
iSize H=540
iVSyncPresentInterval=0
iShadowMapResolution=512
bSAOEnable=0
bScreenSpaceReflectionEnabled=0
bUseTAA=0
[Launcher]
bEnableFileSelection=1
"""
    (documents / "SkyrimPrefs.ini").write_text(prefs)
    local = user / "AppData/Local/Skyrim Special Edition"
    require_contained(local, compat)
    local.mkdir(parents=True, exist_ok=True)
    for name in ("plugins.txt", "loadorder.txt"):
        filename = local / name
        if filename.is_symlink():
            filename.unlink()
    masters = "Skyrim.esm\nUpdate.esm\nDawnguard.esm\nHearthFires.esm\nDragonborn.esm\n"
    (local / "plugins.txt").write_text("*MCParity.esp\n")
    (local / "loadorder.txt").write_text(masters + "MCParity.esp\n")
    evidence = {"scope": "Disposable profile, stock executable, no SKSE/native plugins",
                "game_exe_sha256": hashlib.sha256((game / "SkyrimSE.exe").read_bytes()).hexdigest(),
                "plugin_sha256": hashlib.sha256(plugin()).hexdigest(),
                "pex_sha256": {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                               for p in sorted((data / "Scripts").glob("*.pex"))}}
    (output / "inputs.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(output)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--game", required=True, type=Path)
    parser.add_argument("--pex", required=True, type=Path)
    parser.add_argument("--prefix-template", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    prepare(args.game, args.pex, args.prefix_template, args.output)
