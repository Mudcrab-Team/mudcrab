import copy
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SCRIPT_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPT_DIR))
import experiment
import synthetic


class GlbTests(unittest.TestCase):
    def setUp(self):
        self.document, self.binary = synthetic.grid(12)

    def primitive(self, document=None, binary=None):
        return experiment.extract_primitive(document or self.document, self.binary if binary is None else binary, 0, 0)

    def test_interleaved_attributes_and_source_border_locks(self):
        document, binary = experiment.parse_glb(synthetic.glb(self.document, self.binary))
        primitive = self.primitive(document, binary)
        self.assertEqual(len(primitive["positions"]), 169)
        self.assertEqual(primitive["attribute_count"], 5)
        self.assertEqual(primitive["layout"][0]["semantic"], "NORMAL")
        self.assertEqual(primitive["positions"][0], (0.0, 0.0, 0.0))
        self.assertEqual(sum(primitive["locks"]), 48)
        self.assertEqual(primitive["locks"][13 + 1], 0)
        self.assertEqual(len(primitive["indices"]), 864)

    def test_accessor_cannot_read_neighbor_view(self):
        document = copy.deepcopy(self.document)
        # The bytes still exist in BIN; only the declared vertex view is short.
        document["bufferViews"][0]["byteLength"] -= 8
        with self.assertRaisesRegex(ValueError, "exceeds its buffer view"):
            self.primitive(document)

    def test_index_range_and_nonfinite_positions_fail(self):
        binary = bytearray(self.binary)
        struct.pack_into("<I", binary, self.document["bufferViews"][1]["byteOffset"], 9999)
        with self.assertRaisesRegex(ValueError, "invalid primitive indices"):
            self.primitive(binary=binary)
        binary = bytearray(self.binary)
        struct.pack_into("<f", binary, 0, float("nan"))
        with self.assertRaisesRegex(ValueError, "non-finite"):
            self.primitive(binary=binary)

    def test_normalized_unsigned_color_decode(self):
        document = copy.deepcopy(self.document)
        offset = len(self.binary)
        document["bufferViews"].append({"buffer": 0, "byteOffset": offset, "byteLength": 169 * 4})
        document["accessors"].append({"bufferView": 2, "componentType": 5121, "normalized": True, "count": 169, "type": "VEC4"})
        document["meshes"][0]["primitives"][0]["attributes"]["COLOR_0"] = 4
        primitive = self.primitive(document, self.binary + bytes((255, 128, 0, 255)) * 169)
        self.assertEqual(primitive["attribute_count"], 9)
        self.assertEqual(primitive["attributes"][:4], [1.0, 128 / 255, 0.0, 1.0])

    def test_unsupported_routes_are_explicit(self):
        cases = [
            (lambda d: d["materials"][0].update(alphaMode="MASK"), "alpha-tested"),
            (lambda d: d["nodes"][0].update(skin=0), "skinned"),
            (lambda d: d.update(animations=[{}]), "animated"),
            (lambda d: d["meshes"][0]["primitives"][0].update(targets=[{}]), "morph"),
            (lambda d: d["accessors"][0].update(sparse={}), "sparse"),
            (lambda d: d["meshes"][0]["primitives"][0].update(extensions={"KHR_draco_mesh_compression": {}}), "compressed"),
            (lambda d: d["materials"][0].update(extensions={"KHR_materials_transmission": {}}), "material extensions"),
        ]
        for mutate, reason in cases:
            document = copy.deepcopy(self.document)
            mutate(document)
            with self.subTest(reason=reason), self.assertRaisesRegex(experiment.Omission, reason):
                self.primitive(document)
        document, binary = synthetic.grid(2)
        with self.assertRaisesRegex(experiment.Omission, "small primitive"):
            self.primitive(document, binary)

    def test_degenerate_and_nonmanifold_sources_are_omitted(self):
        with self.assertRaisesRegex(experiment.Omission, "degenerate"):
            experiment.boundary_locks([(0, 0, 0), (1, 0, 0), (2, 0, 0)], [0, 1, 2])
        with self.assertRaisesRegex(experiment.Omission, "non-manifold"):
            experiment.boundary_locks([(0, 0, 0), (1, 0, 0), (0, 1, 0)], [0, 1, 2] * 3)

    def test_material_dependency_cannot_escape_boundary(self):
        document = copy.deepcopy(self.document)
        document["materials"][0]["normalTexture"] = {"index": 0}
        document["textures"] = [{"source": 0}]
        document["images"] = [{"uri": "../outside.ktx2"}]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "assets"
            root.mkdir()
            (root.parent / "outside.ktx2").write_bytes(b"private")
            with self.assertRaisesRegex(experiment.Omission, "escapes asset root"):
                experiment.material_dependencies(document, self.binary, document["materials"][0], root / "source.glb", root)

    def test_standard_specular_material_and_symlink_image_policy(self):
        document = copy.deepcopy(self.document)
        document["materials"][0]["extensions"] = {"KHR_materials_specular": {"specularTexture": {"index": 0}}}
        document["textures"] = [{"source": 0}]
        document["images"] = [{"uri": "texture.ktx2"}]
        self.primitive(document)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            image = root / "texture.ktx2"
            image.write_bytes(b"source texture")
            dependencies, watched = experiment.material_dependencies(document, self.binary, document["materials"][0], root / "source.glb", root)
            self.assertEqual(dependencies[0]["sha256"], experiment.digest(b"source texture"))
            self.assertEqual(watched, {image: experiment.digest(b"source texture")})
            image.unlink()
            target = root / "target.ktx2"
            target.write_bytes(b"source texture")
            image.symlink_to(target.name)
            with self.assertRaisesRegex(experiment.Omission, "symlinked"):
                experiment.material_dependencies(document, self.binary, document["materials"][0], root / "source.glb", root)

    def test_same_image_changes_between_texture_references_reject(self):
        document = copy.deepcopy(self.document)
        document["materials"][0]["normalTexture"] = {"index": 0}
        document["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"] = {"index": 1}
        document["textures"] = [{"source": 0}, {"source": 0}]
        document["images"] = [{"uri": "texture.ktx2"}]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            image = root / "texture.ktx2"
            image.write_bytes(b"generation A")
            original_digest = experiment.file_digest
            calls = 0

            def changing_digest(path):
                nonlocal calls
                calls += 1
                if calls == 2:
                    image.write_bytes(b"generation B")
                return original_digest(path)

            with mock.patch.object(experiment, "file_digest", side_effect=changing_digest):
                with self.assertRaisesRegex(experiment.InputChanged, "source input changed"):
                    experiment.material_dependencies(document, self.binary, document["materials"][0], root / "source.glb", root)
            self.assertEqual(calls, 2)

    def test_duplicate_nonfinite_json_and_header_mismatch_reject(self):
        for value in ('{"a":1,"a":2}', '{"a":NaN}', '{"a":1e999}'):
            with self.assertRaises(ValueError):
                experiment.strict_json(value)
        data = synthetic.glb(self.document, self.binary)
        with self.assertRaisesRegex(ValueError, "header/length"):
            experiment.parse_glb(data[:-1])

    def test_malformed_meshes_and_image_uris_reject(self):
        for mutate in (lambda d: d["meshes"][0].update(primitives=[]),
                       lambda d: d["meshes"][0].update(primitives=[17]),
                       lambda d: d["meshes"][0]["primitives"][0].update(attributes=[]),
                       lambda d: d.update(images=[{"uri": 17}])):
            document = copy.deepcopy(self.document)
            mutate(document)
            with self.assertRaises(ValueError):
                experiment.parse_glb(synthetic.glb(document, self.binary))


def hierarchy():
    bounds = {"center": [0, 0, 0], "radius": 10, "error_estimate": 0.0}
    return {
        "meshoptimizer_commit": experiment.PIN,
        "conventional": [{"target_ratio": 0.5, "error_estimate": 1.0, "indices": [0, 1, 3]}],
        "groups": [
            {"id": 0, "depth": 0, "terminal": False, "simplified_bounds": dict(bounds, error_estimate=1.0),
             "clusters": [{"refines_group": -1, "bounds": bounds.copy(), "indices": [0, 1, 2]},
                          {"refines_group": -1, "bounds": bounds.copy(), "indices": [1, 3, 2]}]},
            {"id": 1, "depth": 1, "terminal": True, "simplified_bounds": dict(bounds, error_estimate=None),
             "clusters": [{"refines_group": 0, "bounds": dict(bounds, error_estimate=1.0), "indices": [0, 1, 3]}]},
        ],
    }


class CutTests(unittest.TestCase):
    primitive = {"indices": [0, 1, 2, 1, 3, 2], "positions": [(0, 0, 0), (1, 0, 0), (0, 1, 0), (1, 1, 0)]}

    def test_group_replacement_and_source_triangle_mapping(self):
        result = experiment.finalize_candidate(hierarchy(), self.primitive)
        self.assertEqual(result["uniform_cuts"], [{"depth": 0, "cluster_ids": [0, 1], "triangle_count": 2}, {"depth": 1, "cluster_ids": [2], "triangle_count": 1}])
        self.assertEqual([cluster["source_triangle_ids"] for cluster in result["clusters"][:2]], [[0], [1]])
        self.assertFalse(result["validation"]["error_bound_certified_conservative"])

    def test_missing_duplicate_and_cyclic_hierarchies_reject(self):
        missing = hierarchy()
        missing["groups"][0]["clusters"].pop()
        duplicate = hierarchy()
        duplicate["groups"][0]["clusters"][1]["indices"] = [0, 1, 2]
        cycle = hierarchy()
        cycle["groups"][1]["clusters"][0]["refines_group"] = 1
        missing_replacement = hierarchy()
        missing_replacement["groups"][0]["terminal"] = True
        missing_replacement["groups"][0]["simplified_bounds"]["error_estimate"] = None
        for data, reason in [(missing, "missing triangles"), (duplicate, "duplicates"), (cycle, "cyclic"), (missing_replacement, "terminal/replacement")]:
            with self.subTest(reason=reason), self.assertRaisesRegex(ValueError, reason):
                experiment.finalize_candidate(data, self.primitive)

    def test_invalid_bounds_and_oversized_cluster_reject(self):
        for field, value, reason in [("radius", -1, "radius"), ("center", [float("nan"), 0, 0], "center"), ("error_estimate", -1, "error")]:
            data = hierarchy()
            data["groups"][0]["clusters"][0]["bounds"][field] = value
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, reason):
                experiment.finalize_candidate(data, self.primitive)
        data = hierarchy()
        data["groups"][0]["clusters"][0]["indices"] = [0, 1, 2] * 129
        with self.assertRaisesRegex(ValueError, "triangle limit"):
            experiment.finalize_candidate(data, self.primitive)


class ParserTests(unittest.TestCase):
    def test_owning_parsers_accept_documented_options_and_reject_typos(self):
        arguments = {"build.py": ["--meshoptimizer", "unused", "--output", "unused"],
                     "experiment.py": ["--input", "unused", "--output", "unused", "--builder", "unused"],
                     "synthetic.py": ["--output", "unused"]}
        for script in arguments:
            with self.subTest(script=script):
                help_result = subprocess.run([sys.executable, str(SCRIPT_DIR / script), "--help"], capture_output=True, text=True)
                self.assertEqual(help_result.returncode, 0, help_result.stderr)
                self.assertIn("--output", help_result.stdout)
                typo = subprocess.run([sys.executable, str(SCRIPT_DIR / script), *arguments[script], "--typo"], capture_output=True, text=True)
                self.assertEqual(typo.returncode, 2)
                self.assertIn("unrecognized arguments: --typo", typo.stderr)


class BuilderFailureTests(unittest.TestCase):
    """Builder failures with a mocked subprocess; no compiler and no real timeout wait."""

    def run_failing(self, root, error):
        assets = root / "assets"
        assets.mkdir(parents=True)
        document, binary = synthetic.grid(12)
        (assets / "grid.glb").write_bytes(synthetic.glb(document, binary))
        builder = root / "builder"
        builder.write_bytes(b"not executed: subprocess.run is mocked\n")
        receipt = {"meshoptimizer_version": "1.3", "meshoptimizer_commit": experiment.PIN,
                   "builder_sha256": experiment.digest(builder.read_bytes()),
                   "builder_source_sha256": "0" * 64}
        Path(str(builder) + ".build.json").write_text(json.dumps(receipt, sort_keys=True))
        with mock.patch.object(experiment.subprocess, "run", side_effect=error):
            manifest = experiment.run_experiment(assets, root / "out", builder)
        return manifest, (root / "out" / "manifest.json").read_bytes()

    def test_nonzero_exit_reason_is_stable_and_keeps_stderr(self):
        stderr = b"meshopt: invalid index range\n"
        results = []
        for label in ("first", "second"):
            with tempfile.TemporaryDirectory() as temporary:
                error = subprocess.CalledProcessError(1, ["builder", "request", "128"], stderr=stderr)
                manifest, data = self.run_failing(Path(temporary) / label, error)
                self.assertNotIn(temporary, data.decode())
                results.append((manifest, data))
        (first, first_bytes), (second, second_bytes) = results
        row = first["files"][0]["primitives"][0]
        self.assertEqual(row["status"], "failed")
        self.assertEqual(row["reason"], "builder exited with status 1: meshopt: invalid index range")
        self.assertEqual(first["counts"], {"candidate": 0, "omitted": 0, "failed": 1})
        self.assertEqual(first_bytes, second_bytes)

    def test_timeout_reason_keeps_partial_stderr_without_waiting(self):
        error = subprocess.TimeoutExpired(["builder"], 120, stderr=b"partial \xff output")
        with tempfile.TemporaryDirectory() as temporary:
            manifest, _ = self.run_failing(Path(temporary), error)
        row = manifest["files"][0]["primitives"][0]
        self.assertEqual(row["status"], "failed")
        self.assertEqual(row["reason"], "builder timed out after 120 s: partial � output")
        self.assertEqual(manifest["counts"]["failed"], 1)

    def test_long_builder_stderr_is_bounded_and_marked(self):
        error = subprocess.CalledProcessError(3, ["builder"], stderr=b"x" * 100_000)
        with tempfile.TemporaryDirectory() as temporary:
            manifest, _ = self.run_failing(Path(temporary), error)
        reason = manifest["files"][0]["primitives"][0]["reason"]
        self.assertTrue(reason.startswith("builder exited with status 3: xxx"))
        self.assertTrue(reason.endswith(" ...[truncated]"))
        self.assertLess(len(reason), experiment.BUILDER_DIAGNOSTIC_LIMIT + 100)

    def test_staging_paths_in_builder_stderr_are_normalized(self):
        captured = {}

        def fail(command, **keyword):
            captured["staging"] = str(Path(command[1]).parent)
            raise subprocess.CalledProcessError(2, command, stderr=f"cannot read {command[1]}\n".encode())

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            assets = root / "assets"
            assets.mkdir()
            document, binary = synthetic.grid(12)
            (assets / "grid.glb").write_bytes(synthetic.glb(document, binary))
            builder = root / "builder"
            builder.write_bytes(b"not executed\n")
            receipt = {"meshoptimizer_version": "1.3", "meshoptimizer_commit": experiment.PIN,
                       "builder_sha256": experiment.digest(builder.read_bytes()), "builder_source_sha256": "0" * 64}
            Path(str(builder) + ".build.json").write_text(json.dumps(receipt, sort_keys=True))
            with mock.patch.object(experiment.subprocess, "run", side_effect=fail):
                manifest = experiment.run_experiment(assets, root / "out", builder)
        reason = manifest["files"][0]["primitives"][0]["reason"]
        self.assertEqual(reason, "builder exited with status 2: cannot read <staging>/primitive.bin")
        self.assertNotIn(captured["staging"], reason)


@unittest.skipUnless(os.environ.get("LOD_EXPERIMENT_BUILDER"), "set LOD_EXPERIMENT_BUILDER to run compiled-adapter fixtures")
class AdapterTests(unittest.TestCase):
    def setUp(self):
        self.builder = Path(os.environ["LOD_EXPERIMENT_BUILDER"])

    def test_malformed_builder_timing_leaves_no_candidate_artifacts(self):
        real_run = subprocess.run

        def malformed_timing(command, **keyword):
            result = real_run(command, **keyword)
            return subprocess.CompletedProcess(result.args, result.returncode, result.stdout, b"{not json")

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            assets = root / "assets"
            assets.mkdir()
            document, binary = synthetic.grid(24)
            (assets / "grid.glb").write_bytes(synthetic.glb(document, binary))
            with mock.patch.object(experiment.subprocess, "run", side_effect=malformed_timing):
                manifest = experiment.run_experiment(assets, root / "out", self.builder, export_controls=True)
            row = manifest["files"][0]["primitives"][0]
            self.assertEqual(row["status"], "failed")
            self.assertEqual(set(row), {"mesh", "primitive", "status", "reason"})
            self.assertEqual(manifest["counts"], {"candidate": 0, "omitted": 0, "failed": 1})
            self.assertNotIn("controls", manifest["files"][0])
            self.assertEqual(os.listdir(root / "out" / "candidates"), [])
            self.assertEqual(json.loads((root / "out" / "measurements.json").read_text())["primitives"], [])

    def test_end_to_end_reproducibility_omissions_and_source_preservation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            assets = root / "assets"
            assets.mkdir()
            document, binary = synthetic.grid(24)
            document["materials"].append({"alphaMode": "MASK"})
            primitive = copy.deepcopy(document["meshes"][0]["primitives"][0])
            primitive["material"] = 1
            document["meshes"][0]["primitives"].append(primitive)
            source_bytes = synthetic.glb(document, binary)
            source = assets / "grid.glb"
            source.write_bytes(source_bytes)
            a = experiment.run_experiment(assets, root / "a", self.builder)
            b = experiment.run_experiment(assets, root / "b", self.builder)
            self.assertEqual(a, b)
            self.assertEqual(a["counts"], {"candidate": 1, "omitted": 1, "failed": 0})
            self.assertEqual(source.read_bytes(), source_bytes)
            relative = a["files"][0]["primitives"][0]["candidate"]
            self.assertEqual((root / "a" / relative).read_bytes(), (root / "b" / relative).read_bytes())
            candidate = json.loads((root / "a" / relative).read_text())
            self.assertEqual(candidate["uniform_cuts"][0]["triangle_count"], 1152)
            self.assertLess(candidate["uniform_cuts"][-1]["triangle_count"], 1152)
            self.assertGreater(len(candidate["groups"]), 1)
            self.assertEqual(candidate["source"]["attribute_layout"][1]["semantic"], "TEXCOORD_0")
            timing = json.loads((root / "a/measurements.json").read_text())["primitives"][0]
            self.assertGreaterEqual(timing["cluster_seconds"], 0)
            self.assertGreater(timing["candidate_json_bytes"], 0)

    def test_renderable_controls_preserve_whole_model_and_portable_images(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            assets = root / "assets"
            assets.mkdir()
            document, binary = synthetic.grid(24)
            document["nodes"][0].update(translation=[1, 2, 3], scale=[2, 3, 4])
            document["materials"].append({"alphaMode": "MASK", "normalTexture": {"index": 0}})
            omitted = copy.deepcopy(document["meshes"][0]["primitives"][0])
            omitted["material"] = 1
            document["meshes"][0]["primitives"].append(omitted)
            document["materials"][0]["normalTexture"] = {"index": 0}
            document["textures"] = [{"source": 0}]
            document["images"] = [{"uri": "texture%20one.png"}]
            import base64
            image_bytes = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            (assets / "texture one.png").write_bytes(image_bytes)
            source = assets / "grid.glb"
            original = synthetic.glb(document, binary)
            source.write_bytes(original)
            output = root / "output"
            manifest = experiment.run_experiment(assets, output, self.builder, export_controls=True)
            self.assertEqual(manifest["counts"], {"candidate": 1, "omitted": 1, "failed": 0})
            controls = manifest["files"][0]["controls"]
            self.assertEqual(controls["status"], "exported")
            candidate = json.loads((output / manifest["files"][0]["primitives"][0]["candidate"]).read_text())
            self.assertEqual(len(controls["artifacts"]), 4 + len(candidate["uniform_cuts"]))
            for artifact in controls["artifacts"]:
                data = (output / artifact["path"]).read_bytes()
                self.assertEqual(experiment.digest(data), artifact["sha256"])
                emitted, payload = experiment.parse_glb(data)
                self.assertEqual(emitted["nodes"], document["nodes"])
                self.assertEqual(emitted["materials"], document["materials"])
                self.assertEqual(emitted["meshes"][0]["primitives"][1], omitted)
                self.assertEqual(payload[:len(binary)], binary)
                self.assertNotIn("uri", emitted["images"][0])
                self.assertEqual(experiment.view_bytes(emitted, payload, emitted["images"][0]["bufferView"])[1], image_bytes)
                primitive = emitted["meshes"][0]["primitives"][0]
                self.assertEqual(primitive["attributes"], document["meshes"][0]["primitives"][0]["attributes"])
                _, indices = experiment.read_accessor(emitted, payload, primitive["indices"])
                if artifact["kind"] == "cluster-depth-0":
                    from collections import Counter
                    source_indices = experiment.extract_primitive(document, binary, 0, 0)["indices"]
                    flattened = [row[0] for row in indices]
                    self.assertEqual(Counter(experiment.oriented_triangle(flattened[i:i + 3]) for i in range(0, len(flattened), 3)),
                                     Counter(experiment.oriented_triangle(source_indices[i:i + 3]) for i in range(0, len(source_indices), 3)))
                if artifact["kind"] == "conventional-0":
                    self.assertEqual([row[0] for row in indices], candidate["conventional"][0]["indices"])
            self.assertEqual(source.read_bytes(), original)
            self.assertEqual((assets / "texture one.png").read_bytes(), image_bytes)
            # Independent verification rejects a changed node transform.
            portable, portable_binary = experiment.embed_control_images(document, binary, source, assets, {})
            changed = copy.deepcopy(portable)
            changed["nodes"][0]["scale"][0] = 9
            with self.assertRaisesRegex(ValueError, "scene/node/material"):
                experiment.verify_control_glb(experiment.encode_glb(changed, portable_binary), document, binary, portable, portable_binary, {})

            corrupted = bytearray(portable_binary)
            corrupted[-1] ^= 1
            with self.assertRaisesRegex(ValueError, "embedded dependency bytes"):
                experiment.verify_control_glb(experiment.encode_glb(portable, bytes(corrupted)),
                    document, binary, portable, portable_binary, {})

    def test_controls_check_images_of_retained_primitives_and_abort_mutation(self):
        for missing in (True, False):
            with self.subTest(missing=missing), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                assets = root / "assets"
                assets.mkdir()
                document, binary = synthetic.grid(24)
                document["materials"].append({"alphaMode": "MASK", "normalTexture": {"index": 0}})
                omitted = copy.deepcopy(document["meshes"][0]["primitives"][0])
                omitted["material"] = 1
                document["meshes"][0]["primitives"].append(omitted)
                document["textures"] = [{"source": 0}]
                document["images"] = [{"uri": "retained.png"}]
                source = assets / "grid.glb"
                source.write_bytes(synthetic.glb(document, binary))
                output = root / "output"
                if missing:
                    manifest = experiment.run_experiment(assets, output, self.builder, export_controls=True)
                    self.assertEqual(manifest["files"][0]["status"], "omitted")
                    self.assertIn("control image is missing", manifest["files"][0]["reason"])
                    self.assertFalse(list(output.rglob("*.glb")))
                else:
                    image = assets / "retained.png"
                    image.write_bytes(b"original image")
                    original = experiment.export_model_controls
                    def mutate_after_export(*args):
                        result = original(*args)
                        image.write_bytes(b"changed image")
                        return result
                    with mock.patch.object(experiment, "export_model_controls", side_effect=mutate_after_export):
                        with self.assertRaisesRegex(ValueError, "source input changed"):
                            experiment.run_experiment(assets, output, self.builder, export_controls=True)
                    self.assertFalse(output.exists())

    def test_invalid_builder_requests_and_new_output_guards(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            request = root / "bad.bin"
            request.write_bytes(b"MCLODX01" + struct.pack("<III", 0xFFFFFFFF, 3, 0))
            result = subprocess.run([str(self.builder), str(request), "128"], capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)
            self.assertIn("counts exceed", result.stderr)
            typo = subprocess.run([str(self.builder), str(request), "128typo"], capture_output=True, text=True)
            self.assertEqual(typo.returncode, 2)
            self.assertIn("must be", typo.stderr)
            document, binary = synthetic.grid()
            source = root / "assets"
            source.mkdir()
            (source / "grid.glb").write_bytes(synthetic.glb(document, binary))
            with self.assertRaisesRegex(ValueError, "disjoint"):
                experiment.run_experiment(source, source / "output", self.builder)
            existing = root / "existing"
            existing.mkdir()
            (existing / "keep").write_bytes(b"keep")
            with self.assertRaisesRegex(ValueError, "new directory"):
                experiment.run_experiment(source, existing, self.builder)
            self.assertEqual((existing / "keep").read_bytes(), b"keep")

    def test_changed_dependency_between_primitives_aborts_publication(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            assets = root / "assets"
            assets.mkdir()
            document, binary = synthetic.grid()
            document["materials"][0]["normalTexture"] = {"index": 0}
            document["textures"] = [{"source": 0}]
            document["images"] = [{"uri": "texture.ktx2"}]
            document["meshes"][0]["primitives"].append(copy.deepcopy(document["meshes"][0]["primitives"][0]))
            (assets / "grid.glb").write_bytes(synthetic.glb(document, binary))
            texture = assets / "texture.ktx2"
            texture.write_bytes(b"generation A")
            calls = 0
            original = experiment.material_dependencies

            def changing_dependency(*args):
                nonlocal calls
                calls += 1
                if calls == 2:
                    texture.write_bytes(b"generation B")
                return original(*args)

            output = root / "output"
            with mock.patch.object(experiment, "material_dependencies", side_effect=changing_dependency):
                with self.assertRaisesRegex(experiment.InputChanged, "source input changed"):
                    experiment.run_experiment(assets, output, self.builder)
            self.assertEqual(calls, 2)
            self.assertFalse(output.exists())

    def test_builder_or_receipt_replacement_aborts_publication(self):
        for changed in ("builder", "receipt"):
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                assets = root / "assets"
                assets.mkdir()
                document, binary = synthetic.grid()
                (assets / "grid.glb").write_bytes(synthetic.glb(document, binary))
                builder = root / "builder"
                builder.write_bytes(self.builder.read_bytes())
                builder.chmod(0o700)
                receipt = Path(str(builder) + ".build.json")
                receipt.write_bytes(Path(str(self.builder) + ".build.json").read_bytes())
                original = experiment.material_dependencies

                def changed_tool(*args):
                    if changed == "builder":
                        builder.write_bytes(b"replacement binary")
                    else:
                        data = json.loads(receipt.read_text())
                        data["generation"] = "replacement"
                        receipt.write_text(json.dumps(data))
                    return original(*args)

                output = root / "output"
                with mock.patch.object(experiment, "material_dependencies", side_effect=changed_tool):
                    with self.assertRaisesRegex(experiment.InputChanged, "builder, receipt"):
                        experiment.run_experiment(assets, output, builder)
                self.assertFalse(output.exists())

    def test_direct_input_symlink_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            document, binary = synthetic.grid()
            source = root / "source.glb"
            source.write_bytes(synthetic.glb(document, binary))
            link = root / "link.glb"
            link.symlink_to(source.name)
            with self.assertRaisesRegex(ValueError, "symlinked GLB"):
                experiment.run_experiment(link, root.parent / "unused-output", self.builder)


if __name__ == "__main__":
    unittest.main()
