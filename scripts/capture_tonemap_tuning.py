#!/usr/bin/env python3
"""Capture named rendering variants against two frozen visual references.

Only the Python standard library is required. Capture validation is deliberately
separate from visual judgement: passing receipts do not establish Skyrim parity.
"""

import argparse
import base64
import datetime
import hashlib
import html
import json
import math
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys


REPOSITORY = Path(__file__).resolve().parents[1]
POSE_KEYS = (
    "camera_creation", "rotation_runtime_xyzw", "vertical_fov_radians", "render_origin",
)
# The runner owns these inputs. A variant must not replace the camera, asset pack,
# screenshot path, INI, automated session, or capture-readiness policy.
RESERVED = {
    "--assets", "--shots", "--shots-out", "--ini", "--start-shot", "--worldspace",
    "--hidden-window", "--matched-route", "--headless",
    "--acceptance-screenshot", "--benchmark-only", "--shots-settle-timeout",
    "--benchmark-frames", "--benchmark-duration", "--streaming-fixture",
    "--material-fixture", "--terrain-water-fixture", "--transform-bounds-fixture",
    "--renderer-fixture", "--physics-fixture",
}
MINIMUM_WORK_COUNTERS = {
    "active_requests", "arming_queue_depth", "asset_load_failures", "failed_cells",
    "failed_lod_work", "loading_cells", "outstanding_terrain_uploads",
    "pending_asset_instances", "pending_batch_cpu", "pending_batch_initial",
    "pending_batch_selection", "pending_lod_chunks", "pending_lod_queries",
    "pending_render_transfers", "pending_specializations", "pending_surface_instances",
    "retiring_cells", "validation_failures",
}
LIMITS = [
    "Approved Mudcrab and Skyrim reference PNGs remain frozen across every stage.",
    "Mudcrab captures use the approved camera, INI, dimensions and pinned water time.",
    "The Skyrim reference's effective weather, exposure, modifiers and GPU state were not observed.",
    "Equal camera receipts do not establish a matched Skyrim projection or native rendering parity.",
    "PNG hashes identify files; repeated native captures can vary despite unchanged settings.",
    "Source and executable hashes are recorded independently; this runner does not attest their build relationship.",
]


def utc_now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, allow_nan=False) + "\n", encoding="utf-8")


def canonical_hash(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, allow_nan=False).encode()).hexdigest()


def png_dimensions(path):
    with Path(path).open("rb") as stream:
        header = stream.read(24)
    if (len(header) != 24 or header[:8] != b"\x89PNG\r\n\x1a\n"
            or header[12:16] != b"IHDR"):
        raise ValueError(f"Not a PNG with an IHDR header: {path}")
    return list(struct.unpack(">II", header[16:24]))


def require(condition, message):
    if not condition:
        raise ValueError(message)


def load_capture_receipt(path):
    records = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.lstrip().startswith("{"):
            value = json.loads(line)
            if "capture" in value and "captured_frame" in value:
                records.append(value)
    require(len(records) == 1, f"{path}: expected exactly one captured-frame receipt")
    return records[0]


def approximately_equal(actual, expected, context):
    if isinstance(expected, list):
        require(isinstance(actual, list) and len(actual) == len(expected), f"{context}: shape changed")
        for index, (left, right) in enumerate(zip(actual, expected)):
            approximately_equal(left, right, f"{context}[{index}]")
    elif isinstance(expected, (int, float)) and not isinstance(expected, bool):
        require(isinstance(actual, (int, float)) and not isinstance(actual, bool)
                and math.isfinite(actual) and math.isclose(actual, expected, rel_tol=0, abs_tol=1e-6),
                f"{context}: {actual!r} differs from frozen {expected!r}")
    else:
        require(actual == expected, f"{context}: {actual!r} differs from frozen {expected!r}")


def assert_expected(actual, expected, context="receipt"):
    """Match the configured subset while retaining every raw field as evidence."""
    if isinstance(expected, dict):
        require(isinstance(actual, dict), f"{context}: expected an object")
        for key, value in expected.items():
            require(key in actual, f"{context}: missing {key}")
            assert_expected(actual[key], value, f"{context}.{key}")
    else:
        approximately_equal(actual, expected, context)


def validate_capture(receipt, baseline, camera, image, expected=None, require_grading=True):
    require(receipt.get("capture") == 0 and receipt.get("kind") == "settled",
            "Capture must be the first and only settled shot")
    require(receipt.get("timed_out") is False, "Capture timed out or has no timeout evidence")
    require(receipt.get("water_time_seconds") == 1.0, "Water time must be pinned to 1.0 seconds")
    dimensions = [camera["width"], camera["height"]]
    require(receipt.get("dimensions") == dimensions == png_dimensions(image), "Capture dimensions changed")
    frame = receipt.get("captured_frame")
    require(isinstance(frame, dict), "Missing captured-frame evidence")
    work = frame.get("work", {})
    require(work.get("renderer_ready") is True, "Captured frame renderer is not ready")
    require(MINIMUM_WORK_COUNTERS <= work.keys(), "Captured frame is missing readiness counters")
    for key, value in work.items():
        if key not in {"quiet_frames", "renderer_ready"}:
            require(type(value) is int and value == 0, f"Captured-frame work remains: {key}={value!r}")
    require(frame.get("pending_specializations") == 0, "Captured-frame specializations are pending or unknown")
    for key in POSE_KEYS:
        require(key in frame and key in baseline["captured_frame"], f"Missing captured-frame pose: {key}")
        approximately_equal(frame[key], baseline["captured_frame"][key], key)
    for requested, captured in (
        ("requested_camera_creation", "camera_creation"),
        ("requested_rotation_runtime_xyzw", "rotation_runtime_xyzw"),
        ("requested_vertical_fov_radians", "vertical_fov_radians"),
    ):
        require(requested in receipt, f"Missing requested pose: {requested}")
        approximately_equal(receipt[requested], frame[captured], requested)
    approximately_equal(frame["camera_creation"], camera["shots"][0]["position"], "camera fixture position")
    shot = camera["shots"][0]
    yaw, pitch = -math.radians(shot["yaw"]) / 2, -math.radians(shot["pitch"]) / 2
    rotation = [math.cos(yaw) * math.sin(pitch), math.sin(yaw) * math.cos(pitch),
                -math.sin(yaw) * math.sin(pitch), math.cos(yaw) * math.cos(pitch)]
    approximately_equal(frame["rotation_runtime_xyzw"], rotation, "camera fixture orientation")
    fov = 2 * math.atan(math.tan(math.radians(shot["hfov"]) / 2) / (camera["width"] / camera["height"]))
    approximately_equal(frame["vertical_fov_radians"], fov, "camera fixture vertical FOV")
    if require_grading:
        grade = frame.get("grading")
        require(isinstance(grade, dict) and isinstance(grade.get("source"), dict),
                "Missing grading/source evidence bound to captured frame")
        for key in ("color_grade", "visual_preset", "preset_saturation", "resolved_grade"):
            require(key in receipt and key in grade and receipt[key] == grade[key],
                    f"Callback and captured-frame {key} disagree")
        require(receipt.get("grading_source") == grade["source"], "Callback and captured-frame source disagree")
        require(grade["source"].get("resolution_error") is None, "Grading source failed to resolve")
    if expected:
        assert_expected(receipt, expected)
    return {
        "passed": True, "camera_matches_approved": True, "dimensions": dimensions,
        "renderer_ready": True, "pending_or_failed_work": 0, "timed_out": False,
        "water_time_seconds": 1.0, "grading_bound_to_captured_frame": require_grading,
    }


def command_options(arguments):
    return [value.split("=", 1)[0] for value in arguments if value.startswith("--")]


def validate_variants(config):
    common = config.get("common_args", [])
    variants = config.get("variants", [])
    require(isinstance(common, list) and all(isinstance(value, str) for value in common),
            "common_args must be an array of strings")
    require(isinstance(variants, list) and variants, "At least one named variant is required")
    options = command_options(common)
    require(not (set(options) & RESERVED), f"common_args cannot replace runner-owned options: {set(options) & RESERVED}")
    require(len(options) == len(set(options)), "common_args contains duplicate options")
    names = set()
    for variant in variants:
        name, arguments = variant.get("name", ""), variant.get("args", [])
        require(isinstance(name, str) and re.fullmatch(r"[a-z0-9][a-z0-9-]*", name),
                "Variant names must use lowercase letters, numbers and hyphens")
        require(name not in names, f"Duplicate variant name: {name}")
        names.add(name)
        require(isinstance(arguments, list) and all(isinstance(value, str) for value in arguments),
                f"{name}: args must be an array of strings")
        selected = command_options(arguments)
        require(not (set(selected) & (RESERVED | set(options))),
                f"{name}: variant replaces a frozen or common option")
        require(len(selected) == len(set(selected)), f"{name}: duplicate variant options")
        require(isinstance(variant.get("expect_receipt", {}), dict), f"{name}: expect_receipt must be an object")


def resolve_input(config_path, value):
    path = Path(value).expanduser()
    return (path if path.is_absolute() else config_path.parent / path).resolve(strict=True)


def freeze_input(config_path, specification, destination):
    require(isinstance(specification, dict), f"Input needs path and sha256: {destination.name}")
    source = resolve_input(config_path, specification["path"])
    expected = specification.get("sha256", "")
    require(isinstance(expected, str) and re.fullmatch(r"[a-f0-9]{64}", expected),
            f"Pin a lowercase SHA256 for {source}")
    require(sha256(source) == expected, f"Frozen input hash changed: {source}")
    shutil.copyfile(source, destination)
    require(sha256(destination) == expected, f"Copied input hash changed: {source}")
    return {"original_path": str(source), "path": str(destination), "sha256": expected}


def git_output(repository, *arguments):
    result = subprocess.run(["git", *arguments], cwd=repository, capture_output=True, check=True)
    return result.stdout


def source_snapshot(repository, source_paths):
    files = {}
    for value in source_paths:
        selected = repository / value
        require(selected.exists(), f"Missing source provenance path: {selected}")
        candidates = sorted(selected.rglob("*")) if selected.is_dir() else [selected]
        for candidate in candidates:
            if candidate.is_file():
                files[str(candidate.relative_to(repository))] = sha256(candidate)
    result = {
        "git_head": git_output(repository, "rev-parse", "HEAD").decode().strip(),
        "git_status": git_output(repository, "status", "--short").decode(),
        "working_tree_diff_sha256": hashlib.sha256(git_output(repository, "diff", "HEAD", "--binary")).hexdigest(),
        "files": files,
    }
    result["sha256"] = canonical_hash(result)
    return result


def image_data(path):
    return "data:image/png;base64," + base64.b64encode(path.read_bytes()).decode("ascii")


def preview_provenance(value):
    """Keep textual image evidence from being mistaken for displayed images.

    T3 inlines absolute PNG paths throughout a page, including JSON provenance.
    Only the preview's actual image fields should trigger that operation.
    """
    if isinstance(value, dict):
        return {key: preview_provenance(item) for key, item in value.items()}
    if isinstance(value, list):
        return [preview_provenance(item) for item in value]
    if isinstance(value, str) and value.lower().endswith(".png") and Path(value).is_absolute():
        try:
            return str(Path(value).relative_to(REPOSITORY))
        except ValueError:
            return "Local artifact: " + Path(value).name
    return value


def render_gallery(manifest, output):
    approved = output / "references/approved.png"
    skyrim = output / "references/skyrim.png"
    runs = [run for run in manifest["runs"] if run.get("validation", {}).get("passed")]
    if not runs:
        return
    data = {
        "approved": image_data(approved), "skyrim": image_data(skyrim),
        "references": manifest["inputs"], "limits": manifest["limits"], "runs": [],
    }
    for run in runs:
        receipt = json.loads((output / run["receipt_file"]).read_text())
        data["runs"].append({
            "name": run["name"], "label": run.get("label", run["name"]),
            "notes": run.get("notes", ""), "image": image_data(output / run["image_file"]),
            "sha256": run["image_sha256"], "engine_sha256": run["engine_sha256"],
            "source_sha256": run["source_sha256"], "command": run["command"],
            "validation": run["validation"], "grading_source": receipt.get("grading_source"),
            "resolved_grade": receipt.get("resolved_grade"),
        })
    title = html.escape(manifest.get("title", "Tone-map tuning"))
    page = r'''<!doctype html>
<html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>__TITLE__</title>
<style>
*{box-sizing:border-box}body{margin:0;width:100%;color:var(--foreground,#17262f);font:15px/1.5 var(--font-sans,system-ui,sans-serif)}
p{margin:8px 0}label{display:inline-flex;align-items:center;gap:9px;max-width:100%;min-width:0}select,button{font:inherit;color:inherit;background:var(--secondary,#eef3f5);border:1px solid var(--border,#b7c8d1);border-radius:var(--radius,7px);padding:7px 11px;max-width:100%;min-width:0}
button{cursor:pointer}button[aria-pressed=true]{background:var(--accent-surface,#dcecf2);border-color:var(--ring,#6998a9)}.muted{color:var(--muted-foreground,#536c7a);font-size:13px}.controls{display:flex;flex-wrap:wrap;align-items:center;gap:13px;margin:18px 0}
.triptych{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:12px}.triptych figure{margin:0;min-width:0}.triptych img{display:block;width:100%;height:auto}.triptych figcaption{font-size:13px;margin:7px 0}.caption{color:var(--muted-foreground,#536c7a)}
.viewer{position:relative;width:100%;aspect-ratio:var(--aspect);overflow:hidden;border:1px solid var(--border,#b7c8d1)}.viewer img{position:absolute;width:100%;height:100%;object-fit:contain;inset:0}.viewer .split{position:absolute;top:0;bottom:0;left:50%;width:2px;background:#fff;pointer-events:none}
.viewer .tag{position:absolute;top:10px;color:#fff;background:#101920dd;border-radius:4px;padding:4px 9px;font-size:12px;max-width:45%}.left{left:10px}.right{right:10px}.wipe{width:240px;max-width:45vw}details{border-top:1px solid var(--border,#b7c8d1);padding:13px 0;margin-top:14px}summary{cursor:pointer}pre{white-space:pre-wrap;overflow-wrap:anywhere;background:var(--code-background,#eef3f5);color:var(--code-foreground,inherit);padding:16px;border-radius:var(--radius,7px);font:12px/1.5 var(--font-mono,ui-monospace,monospace)}ul{padding-left:22px}.status{color:var(--success-foreground,#286140)}.note{padding:16px;background:var(--card,#eef3f5);border-left:3px solid var(--ring,#6998a9);border-radius:var(--radius,7px)}
@media(max-width:600px){.triptych{gap:6px}.triptych figcaption{font-size:11px}.controls{gap:9px}.viewer .tag{font-size:10px}}
</style>
<p class="muted">Frozen references · original PNG colors · equal image scale · no crops</p>
<div class="controls"><label>Current stage <select id="stage"></select></label><button id="previous">Previous stage</button><button id="next">Next stage</button><span class="status">Capture receipts passed</span></div>
<p id="stageNote" class="note"></p>
<div class="triptych">
<figure><img id="approved" alt="Previous approved Mudcrab setup"><figcaption>Previous approved setup <span class="caption">· frozen</span></figcaption></figure>
<figure><img id="skyrim" alt="Frozen Skyrim reference"><figcaption>Skyrim reference <span class="caption">· frozen</span></figcaption></figure>
<figure><img id="current" alt="Current Mudcrab tuning stage"><figcaption id="currentLabel"></figcaption></figure>
</div>
<div class="controls"><label>Compare current against <select id="base"><option value="approved">Approved setup</option><option value="skyrim">Skyrim reference</option><option value="previous">Previous tuning stage</option></select></label>
<label>Wipe <input id="wipe" class="wipe" type="range" min="0" max="100" value="50"></label>
<button id="showA" aria-pressed="false">A · reference</button><button id="showB" aria-pressed="false">B · current</button><button id="showWipe" aria-pressed="true">Wipe</button></div>
<div class="viewer" id="viewer"><img id="baseImage" alt="A reference"><img id="currentImage" alt="B current"><div class="split" id="split"></div><span class="tag left" id="baseLabel"></span><span class="tag right" id="variantLabel"></span></div>
<p class="muted">Both views use the same fitted scale. Choose A or B to flip the whole frame; wipe exposes differences in place.</p>
<details><summary>Current capture and rendering evidence</summary><pre id="evidence"></pre></details>
<details><summary>Frozen input hashes and comparison limits</summary><pre id="inputs"></pre><ul id="limits"></ul></details>
<script id="comparison-data" type="application/json">__PAYLOAD__</script>
<script>
'use strict';
const data=JSON.parse(document.getElementById('comparison-data').textContent), $=id=>document.getElementById(id);
data.runs.forEach((run,i)=>{const option=document.createElement('option');option.value=i;option.textContent=run.label;$('stage').append(option)});
$('approved').src=data.approved;$('skyrim').src=data.skyrim;
$('inputs').textContent=JSON.stringify(data.references,null,2);
data.limits.forEach(text=>{const li=document.createElement('li');li.textContent=text;$('limits').append(li)});
let view='wipe';
function draw(){
 const i=Number($('stage').value),run=data.runs[i],base=$('base').value;
 const source=base==='skyrim'?{image:data.skyrim,label:'Skyrim reference'}:base==='previous'&&i>0?data.runs[i-1]:{image:data.approved,label:'Previous approved setup'};
 $('current').src=run.image;$('currentLabel').textContent=run.label+' · current';
 $('baseImage').src=source.image;$('currentImage').src=run.image;
 $('baseLabel').textContent='A · '+source.label;$('variantLabel').textContent='B · '+run.label;
 $('stageNote').textContent=run.notes||'Same frozen references and Mudcrab scene inputs; assess this stage before changing another rendering variable.';
 const p=view==='a'?100:view==='b'?0:Number($('wipe').value);
 $('currentImage').style.clipPath=`inset(0 0 0 ${p}%)`;$('split').style.left=p+'%';$('split').hidden=view!=='wipe';
 $('baseLabel').hidden=view==='b';$('variantLabel').hidden=view==='a';
 $('showA').setAttribute('aria-pressed',view==='a');$('showB').setAttribute('aria-pressed',view==='b');$('showWipe').setAttribute('aria-pressed',view==='wipe');
 $('previous').disabled=i===0;$('next').disabled=i===data.runs.length-1;
 const {image,...evidence}=run;$('evidence').textContent=JSON.stringify(evidence,null,2);
}
$('stage').onchange=draw;$('base').onchange=draw;$('wipe').oninput=()=>{view='wipe';draw()};
$('showA').onclick=()=>{view='a';draw()};$('showB').onclick=()=>{view='b';draw()};$('showWipe').onclick=()=>{view='wipe';draw()};
$('previous').onclick=()=>{$('stage').value=Number($('stage').value)-1;draw()};$('next').onclick=()=>{$('stage').value=Number($('stage').value)+1;draw()};
$('viewer').style.setProperty('--aspect',data.runs[0].validation.dimensions.join('/'));draw();
window.addEventListener('load',async()=>{
 const checks={},originalApproved=$('approved').src,originalSkyrim=$('skyrim').src;
 if(data.runs.length>1){$('stage').value=1;draw();checks.stageSwitch=$('currentLabel').textContent===data.runs[1].label+' · current'}else checks.stageSwitch='single stage';
 $('showA').click();checks.showA=$('currentImage').style.clipPath==='inset(0px 0px 0px 100%)';
 $('showB').click();checks.showB=$('currentImage').style.clipPath==='inset(0px 0px 0px 0%)';
 $('wipe').value=37;$('wipe').dispatchEvent(new Event('input'));checks.wipe=$('currentImage').style.clipPath==='inset(0px 0px 0px 37%)';
 $('base').value='skyrim';draw();checks.skyrimComparison=$('baseImage').src===originalSkyrim;
 $('base').value='previous';draw();checks.previousStageComparison=$('baseImage').getAttribute('src')===(data.runs.length>1?data.runs[0].image:data.approved);
 // Restore the first stage before measuring; the document's initial view remains unchanged.
 $('stage').value=0;$('base').value='approved';$('wipe').value=50;view='wipe';draw();
 checks.referencesFrozen=$('approved').src===originalApproved&&$('skyrim').src===originalSkyrim;
 const dims=data.runs[0].validation.dimensions,images=[$('approved'),$('skyrim'),$('current')];
 await Promise.all(images.map(img=>img.decode().catch(()=>{})));
 checks.imageDimensions=images.every(img=>img.naturalWidth===dims[0]&&img.naturalHeight===dims[1]);
 const widths=images.map(img=>img.getBoundingClientRect().width);checks.equalImageScale=Math.max(...widths)-Math.min(...widths)<1;
 const box=$('viewer').getBoundingClientRect();checks.viewerAspect=Math.abs(box.width/box.height-dims[0]/dims[1])<0.02;
 checks.noHorizontalOverflow=document.documentElement.scrollWidth<=window.innerWidth;
 console.log('Tone-map comparison checks: '+JSON.stringify(checks));
 if(Object.values(checks).some(value=>value===false))console.error('Tone-map comparison self-check failed');
});
</script></html>'''
    for filename, local_paths in (("comparison.html", False), ("comparison-preview.html", True)):
        if local_paths:
            data = preview_provenance(data)
            data["approved"], data["skyrim"] = str(approved), str(skyrim)
            for entry, run in zip(data["runs"], runs):
                entry["image"] = str(output / run["image_file"])
        payload = json.dumps(data, allow_nan=False).replace("<", "\\u003c")
        (output / filename).write_text(page.replace("__TITLE__", title).replace("__PAYLOAD__", payload), encoding="utf-8")
    # Each inline stage contains precisely two frozen references and one current
    # image. Keep the complete multi-stage gallery above for offline review.
    for entry in data["runs"]:
        stage_data = {**data, "runs": [entry]}
        payload = json.dumps(stage_data, allow_nan=False).replace("<", "\\u003c")
        filename = f"comparison-{entry['name']}-preview.html"
        (output / filename).write_text(page.replace("__TITLE__", title).replace("__PAYLOAD__", payload), encoding="utf-8")


def verify_frozen_inputs(manifest):
    for name, item in manifest["inputs"].items():
        require(sha256(Path(item["path"])) == item["sha256"], f"Frozen {name} changed")


def verify_asset_inputs(manifest):
    for name, item in manifest["asset_provenance"].items():
        stat = Path(item["path"]).stat()
        require(stat.st_size == item["size"] and stat.st_mtime_ns == item["mtime_ns"],
                f"Asset provenance input changed during tuning: {name}")


def validate_existing(output):
    manifest_path = output / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    verify_frozen_inputs(manifest)
    camera = json.loads(Path(manifest["inputs"]["camera"]["path"]).read_text())
    baseline = load_capture_receipt(Path(manifest["inputs"]["approved_receipt"]["path"]))
    source = json.loads((output / manifest["source_provenance_file"]).read_text())
    source_hash = source.pop("sha256")
    require(canonical_hash(source) == source_hash == manifest["source_sha256"], "Source provenance changed")
    require(len(manifest["runs"]) == len(manifest["variants"]), "Capture sequence is incomplete")
    for run, variant in zip(manifest["runs"], manifest["variants"]):
        require(run["name"] == variant["name"] and run.get("exit_code") == 0, "A variant did not finish successfully")
        require(run["engine_sha256"] == manifest["engine_sha256"] and run["source_sha256"] == source_hash,
                "Variant engine/source provenance differs from this run")
        require(run["frozen_inputs_sha256"] == {name: item["sha256"] for name, item in manifest["inputs"].items()},
                "Variant frozen input provenance differs from this run")
        receipt_path = output / run["receipt_file"]
        image = output / run["image_file"]
        require(sha256(image) == run["image_sha256"], f"Captured PNG changed: {run['name']}")
        require(sha256(receipt_path) == run["receipt_sha256"], f"Captured receipt changed: {run['name']}")
        receipt = json.loads(receipt_path.read_text())
        run["validation"] = validate_capture(receipt, baseline, camera, image, variant.get("expect_receipt"))
    manifest["passed"] = True
    manifest["last_validated_utc"] = utc_now()
    write_json(manifest_path, manifest)
    render_gallery(manifest, output)
    return manifest


def capture(config_path, engine, assets, output):
    config = json.loads(config_path.read_text(encoding="utf-8"))
    require(config.get("schema_version") == 1, "Expected config schema_version 1")
    validate_variants(config)
    output.mkdir(parents=True, exist_ok=False)
    references = output / "references"
    references.mkdir()
    inputs = {}
    for name, filename in (
        ("approved", "approved.png"), ("skyrim", "skyrim.png"),
        ("approved_receipt", "approved-shots.log"), ("camera", "camera.json"), ("ini", "capture.ini"),
    ):
        inputs[name] = freeze_input(config_path, config[name], references / filename)
    for name, specification in config.get("files", {}).items():
        require(re.fullmatch(r"[a-z0-9][a-z0-9-]*", name) is not None and name not in inputs,
                "Additional input names must be unique lowercase letters, numbers and hyphens")
        suffix = Path(specification["path"]).suffix
        inputs[name] = freeze_input(config_path, specification, references / f"input-{name}{suffix}")
    camera = json.loads(Path(inputs["camera"]["path"]).read_text())
    require(len(camera.get("shots", [])) == 1, "Tuning requires exactly one fixed camera shot")
    shot_name = camera["shots"][0].get("name", "")
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]*", shot_name) is not None, "Unsafe camera shot name")
    dimensions = [camera["width"], camera["height"]]
    for name in ("approved", "skyrim"):
        require(png_dimensions(Path(inputs[name]["path"])) == dimensions, f"{name}: reference dimensions differ")
    baseline = load_capture_receipt(Path(inputs["approved_receipt"]["path"]))
    validate_capture(baseline, baseline, camera, Path(inputs["approved"]["path"]), require_grading=False)
    source_paths = config.get("source_paths", ["Cargo.toml", "Cargo.lock", "crates/engine/src", "crates/shared/src"])
    timeout = config.get("capture_timeout_seconds", 600)
    settle_timeout = config.get("settle_timeout_seconds", 300)
    require(type(timeout) is int and timeout > 0 and type(settle_timeout) is int and settle_timeout > 0,
            "Capture and settle timeouts must be positive integer seconds")
    engine_hash = sha256(engine)
    source = source_snapshot(REPOSITORY, source_paths)
    write_json(output / "source-provenance.json", source)
    asset_files = {}
    for name in ("skyrim_world.db", "conversion-manifest.json", "lod-manifest.json"):
        candidate = assets / name
        if candidate.is_file():
            stat = candidate.stat()
            asset_files[name] = {"path": str(candidate), "sha256": sha256(candidate),
                                 "size": stat.st_size, "mtime_ns": stat.st_mtime_ns}
    manifest = {
        "schema_version": 1, "kind": "mudcrab-tonemap-tuning", "title": config.get("title", "Tone-map tuning"),
        "started_utc": utc_now(), "config_original_path": str(config_path),
        "config_sha256": sha256(config_path), "inputs": inputs, "limits": LIMITS,
        "engine": str(engine), "engine_sha256": engine_hash,
        "engine_mtime_ns": engine.stat().st_mtime_ns, "assets": str(assets), "asset_provenance": asset_files,
        "asset_provenance_scope": "World database and present conversion/LOD manifests; remaining asset files are not hashed.",
        "source_sha256": source["sha256"], "source_provenance_file": "source-provenance.json",
        "source_paths": source_paths, "common_args": config.get("common_args", []),
        "variants": config["variants"], "runs": [], "passed": False,
    }
    shutil.copyfile(config_path, output / "config.json")
    manifest_path = output / "manifest.json"
    write_json(manifest_path, manifest)
    for variant in config["variants"]:
        verify_frozen_inputs(manifest)
        verify_asset_inputs(manifest)
        require(sha256(engine) == engine_hash, "Engine executable changed between variants")
        before = source_snapshot(REPOSITORY, source_paths)
        require(before["sha256"] == source["sha256"], "Source changed between variants; start a fresh tuning run")
        directory = output / variant["name"]
        directory.mkdir()
        arguments = []
        for argument in [*config.get("common_args", []), *variant.get("args", [])]:
            if argument.startswith("{input:") and argument.endswith("}"):
                input_name = argument[7:-1]
                require(input_name in inputs, f"Unknown frozen input placeholder: {argument}")
                argument = inputs[input_name]["path"]
            arguments.append(argument)
        command = [str(engine), "--assets", str(assets), "--shots", inputs["camera"]["path"],
                   "--ini", inputs["ini"]["path"], "--shots-out", str(directory / "capture"),
                   "--hidden-window", "--shots-settle-timeout", str(settle_timeout),
                   *arguments]
        run = {
            "name": variant["name"], "label": variant.get("label", variant["name"]),
            "notes": variant.get("notes", ""), "command": command, "started_utc": utc_now(),
            "engine_sha256": engine_hash, "source_sha256": before["sha256"],
            "frozen_inputs_sha256": {name: item["sha256"] for name, item in inputs.items()},
        }
        manifest["runs"].append(run)
        write_json(manifest_path, manifest)
        print(f"Capturing {variant['name']}: {directory}", flush=True)
        try:
            with (directory / "native.log").open("w", encoding="utf-8") as log:
                result = subprocess.run(command, cwd=REPOSITORY, stdout=log, stderr=subprocess.STDOUT, timeout=timeout)
            run["exit_code"] = result.returncode
            require(result.returncode == 0, f"{variant['name']}: engine exited {result.returncode}; inspect native.log")
            require(sha256(engine) == engine_hash, "Engine executable changed during capture")
            require(source_snapshot(REPOSITORY, source_paths)["sha256"] == before["sha256"],
                    "Source changed during capture")
            verify_frozen_inputs(manifest)
            verify_asset_inputs(manifest)
            receipt = load_capture_receipt(directory / "capture/shots.log")
            image = directory / f"capture/{shot_name}.png"
            run["validation"] = validate_capture(receipt, baseline, camera, image, variant.get("expect_receipt"))
            run["image_file"] = str(image.relative_to(output))
            run["image_sha256"] = sha256(image)
            receipt_path = directory / "receipt.json"
            write_json(receipt_path, receipt)
            run["receipt_file"] = str(receipt_path.relative_to(output))
            run["receipt_sha256"] = sha256(receipt_path)
        except (ValueError, subprocess.TimeoutExpired, OSError) as error:
            run["error"] = str(error)
            raise
        finally:
            run["finished_utc"] = utc_now()
            write_json(directory / "run.json", run)
            write_json(manifest_path, manifest)
            render_gallery(manifest, output)
        print(f"Validated {variant['name']}; comparison.html updated", flush=True)
    manifest["passed"] = True
    manifest["finished_utc"] = utc_now()
    write_json(manifest_path, manifest)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, help="Pinned inputs and named variant arguments")
    parser.add_argument("--engine", type=Path, help="Already-built native engine executable")
    parser.add_argument("--assets", type=Path, help="Converted assets used for every variant")
    parser.add_argument("--output", type=Path, help="Fresh output directory; existing paths are refused")
    parser.add_argument("--validate-only", type=Path, metavar="OUTPUT", help="Revalidate and rebuild an existing gallery without launching")
    args = parser.parse_args()
    try:
        if args.validate_only:
            require(not any([args.config, args.engine, args.assets, args.output]),
                    "--validate-only cannot be combined with capture arguments")
            manifest = validate_existing(args.validate_only.resolve(strict=True))
        else:
            require(all([args.config, args.engine, args.assets, args.output]),
                    "Capture requires --config, --engine, --assets and --output")
            manifest = capture(args.config.resolve(strict=True), args.engine.resolve(strict=True),
                               args.assets.resolve(strict=True), args.output.resolve())
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as error:
        parser.exit(1, f"capture_tonemap_tuning: {error}\n")
    print(f"Passed {len(manifest['runs'])} tuning stages; frozen references verified", flush=True)


if __name__ == "__main__":
    main()
