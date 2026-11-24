import unittest

from compare_traces import CASES, compare, retail_traces


def inputs():
    traces = {name: [f"MCPARITY|{name}|begin", f"MCPARITY|{name}|done"] for name in CASES}
    traces["latent"] = ["MCPARITY|latent|" + marker for marker in
                        ("begin", "outer.before", "inner.before", "inner.after", "outer.after=42", "done")]
    luau = {"cases": {name: {"trace": trace, "error": None, "finished": True}
                      for name, trace in traces.items()},
            "pex_sha256": {"fixture.pex": "same"}, "helper_sha256": "helper",
            "probe_only_wait_adapter": {"trace": traces["latent"], "error": None, "finished": True,
                                        "yields": [{"kind": "Wait", "seconds": 0.5,
                                                    "trace_before_resume": traces["latent"][:3]}]}}
    deployed = {"pex_sha256": {"fixture.pex": "same"}, "game_exe_sha256": "game", "plugin_sha256": "plugin"}
    text = "\n".join("[date] " + trace[i] for i in range(6) for trace in traces.values() if i < len(trace))
    return text, luau, deployed


class ComparisonTests(unittest.TestCase):
    def test_interleaved_quests_compare_per_case(self):
        self.assertEqual(compare(*inputs())["status"], "match")

    def test_missing_or_partial_retail_execution_is_not_a_pass(self):
        with self.assertRaisesRegex(ValueError, "Incomplete"):
            retail_traces("[date] MCPARITY|inheritance|begin")

    def test_duplicate_execution_is_not_silently_selected(self):
        text, _, _ = inputs()
        with self.assertRaisesRegex(ValueError, "Multiple"):
            retail_traces(text + "\n" + text)

    def test_changed_bytecode_cannot_claim_parity(self):
        text, luau, deployed = inputs()
        deployed["pex_sha256"]["fixture.pex"] = "different"
        with self.assertRaisesRegex(ValueError, "identical PEX"):
            compare(text, luau, deployed)

    def test_semantic_difference_reports_its_first_position(self):
        text, luau, deployed = inputs()
        luau["cases"]["inheritance"]["trace"][0] += ".different"
        result = compare(text, luau, deployed)
        self.assertEqual(result["cases"]["inheritance"]["status"], "mismatch")
        self.assertEqual(result["cases"]["inheritance"]["first_difference"], 0)

    def test_probe_wait_adapter_does_not_hide_missing_native(self):
        text, luau, deployed = inputs()
        luau["cases"]["latent"]["error"] = "unregistered Papyrus native: Utility::Wait"
        luau["cases"]["latent"]["finished"] = False
        result = compare(text, luau, deployed)
        self.assertEqual(result["cases"]["latent"]["status"], "unsupported_native")
        self.assertEqual(result["status"], "differences")
        self.assertTrue(result["probe_only_wait_adapter_matches"])

    def test_probe_wait_requires_one_yield_at_the_pause(self):
        for yields in ([], [{"kind": "Wait", "seconds": 0.5, "trace_before_resume": []}],
                       [{"kind": "Wait", "seconds": 0.5, "trace_before_resume": ["too late"]}]):
            with self.subTest(yields=yields):
                text, luau, deployed = inputs()
                luau["probe_only_wait_adapter"]["yields"] = yields
                self.assertFalse(compare(text, luau, deployed)["probe_only_wait_adapter_matches"])

    def test_probe_wait_requires_the_expected_argument_and_count(self):
        for seconds, count in ((1.0, 1), (0.5, 2)):
            with self.subTest(seconds=seconds, count=count):
                text, luau, deployed = inputs()
                token = luau["probe_only_wait_adapter"]["yields"][0]
                token["seconds"] = seconds
                luau["probe_only_wait_adapter"]["yields"] = [token] * count
                self.assertFalse(compare(text, luau, deployed)["probe_only_wait_adapter_matches"])


if __name__ == "__main__":
    unittest.main()
