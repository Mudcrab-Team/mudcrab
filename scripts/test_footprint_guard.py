"""Tests for the footprint guard. The policy tests start no process.

The one integration test (macOS only) starts a small Python child that holds about 300 MiB
and checks that a 128 MiB limit kills it. It opens no window and uses no GPU.
"""

import importlib.util
import subprocess
import sys
import textwrap
import time
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("footprint_guard.py")
SPEC = importlib.util.spec_from_file_location("footprint_guard", SCRIPT)
guard = importlib.util.module_from_spec(SPEC)
sys.modules["footprint_guard"] = guard  # dataclasses needs the module registered
SPEC.loader.exec_module(guard)

GIB = guard.GIB


def sample(footprint_gib, rss_gib=0.0, pids=(100,)):
    return guard.Sample(int(footprint_gib * GIB), int(rss_gib * GIB), tuple(pids))


class DecideTests(unittest.TestCase):
    config = guard.Config(limit_bytes=20 * GIB)

    def test_below_limit_continues(self):
        verdict = guard.decide(self.config, 1.0, sample(19.99), 0)
        self.assertEqual(verdict.action, "continue")

    def test_one_byte_below_limit_continues(self):
        s = guard.Sample(20 * GIB - 1, 0, (1,))
        self.assertEqual(guard.decide(self.config, 1.0, s, 0).action, "continue")

    def test_exactly_at_limit_kills(self):
        s = guard.Sample(20 * GIB, 0, (1,))
        verdict = guard.decide(self.config, 1.0, s, 0)
        self.assertEqual(verdict.action, "kill")
        self.assertEqual(verdict.exit_code, guard.EXIT_OVER_LIMIT)

    def test_above_limit_kills(self):
        verdict = guard.decide(self.config, 1.0, sample(34.5), 0)
        self.assertEqual((verdict.action, verdict.exit_code), ("kill", guard.EXIT_OVER_LIMIT))

    def test_rss_is_never_compared(self):
        # The measured late state: 14.1 GiB RSS but 34.5 GiB footprint kills. The inverse
        # case, high RSS with low footprint, must not kill.
        self.assertEqual(guard.decide(self.config, 1.0, sample(34.5, rss_gib=14.1), 0).action, "kill")
        self.assertEqual(guard.decide(self.config, 1.0, sample(4.0, rss_gib=30.0), 0).action, "continue")

    def test_failed_sample_kills_at_once_by_default(self):
        verdict = guard.decide(self.config, 1.0, None, 1)
        self.assertEqual((verdict.action, verdict.exit_code), ("kill", guard.EXIT_SAMPLE_FAILURE))

    def test_failed_sample_can_be_tolerated_a_set_number_of_times(self):
        config = guard.Config(limit_bytes=20 * GIB, max_sample_failures=3)
        self.assertEqual(guard.decide(config, 1.0, None, 2).action, "continue")
        self.assertEqual(guard.decide(config, 1.0, None, 3).action, "kill")

    def test_gone_target_exits_without_kill(self):
        verdict = guard.decide(self.config, 1.0, None, 0, gone=True)
        self.assertEqual((verdict.action, verdict.exit_code), ("exit", guard.EXIT_GONE))

    def test_deadline_kills_but_limit_takes_priority(self):
        config = guard.Config(limit_bytes=20 * GIB, deadline_s=60)
        self.assertEqual(guard.decide(config, 59.9, sample(1), 0).action, "continue")
        late = guard.decide(config, 60.0, sample(1), 0)
        self.assertEqual((late.action, late.exit_code), ("kill", guard.EXIT_DEADLINE))
        both = guard.decide(config, 61.0, sample(25), 0)
        self.assertEqual(both.exit_code, guard.EXIT_OVER_LIMIT)


class ConfigTests(unittest.TestCase):
    def test_limit_above_20_gib_is_refused(self):
        with self.assertRaises(guard.Refused):
            guard.Config(limit_bytes=20 * GIB + 1).validate()

    def test_lower_limit_is_allowed(self):
        guard.Config(limit_bytes=8 * GIB).validate()

    def test_zero_or_negative_limit_is_refused(self):
        for limit in (0, -1):
            with self.assertRaises(guard.Refused):
                guard.Config(limit_bytes=limit).validate()

    def test_interval_must_be_at_most_100_ms(self):
        guard.Config(interval_s=0.1).validate()
        with self.assertRaises(guard.Refused):
            guard.Config(interval_s=0.101).validate()
        with self.assertRaises(guard.Refused):
            guard.Config(interval_s=0.001).validate()

    def test_bad_deadline_and_failure_count_are_refused(self):
        with self.assertRaises(guard.Refused):
            guard.Config(deadline_s=0).validate()
        with self.assertRaises(guard.Refused):
            guard.Config(max_sample_failures=0).validate()


class DescendantsTests(unittest.TestCase):
    def test_collects_the_subtree_root_first(self):
        rows = [(2, 1), (3, 2), (4, 2), (5, 3), (9, 8), (10, 9)]
        self.assertEqual(guard.descendants(rows, 2), [2, 3, 4, 5])

    def test_ignores_unrelated_processes(self):
        self.assertEqual(guard.descendants([(9, 8)], 2), [2])

    def test_is_safe_on_a_cycle(self):
        self.assertEqual(guard.descendants([(2, 3), (3, 2)], 2), [2, 3])


class FakeClock:
    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class WatchTests(unittest.TestCase):
    def run_watch(self, script, config=None, pids=(100, 101)):
        """`script` is a list of Sample, SampleError, or ProcessGone, one per poll."""
        clock = FakeClock()
        steps = iter(script)
        killed = []
        rows = []

        def sample_fn(root):
            step = next(steps)
            if isinstance(step, Exception):
                raise step
            return step

        result = guard.watch(
            100,
            config or guard.Config(),
            sample_fn,
            killed.append,
            clock=clock,
            sleep=clock.sleep,
            log=rows.append,
        )
        return result, killed, rows

    def test_kills_the_whole_tree_when_footprint_crosses_the_limit(self):
        script = [sample(4), sample(12, pids=(100, 101)), sample(21, pids=(100, 101, 102))]
        result, killed, rows = self.run_watch(script)
        self.assertEqual(result.verdict.exit_code, guard.EXIT_OVER_LIMIT)
        self.assertEqual(killed, [100, 101, 102])  # root first
        self.assertEqual(result.killed, [100, 101, 102])
        self.assertEqual(result.samples, 3)
        self.assertAlmostEqual(result.peak_footprint / GIB, 21, places=3)
        self.assertEqual(rows[-1]["action"], "kill")

    def test_exits_without_kill_when_the_target_ends(self):
        result, killed, _ = self.run_watch([sample(4), sample(5), guard.ProcessGone(100)])
        self.assertEqual(result.verdict.exit_code, guard.EXIT_GONE)
        self.assertEqual(killed, [])

    def test_failed_read_kills_the_last_known_tree(self):
        script = [sample(4, pids=(100, 101)), guard.SampleError("boom")]
        result, killed, _ = self.run_watch(script)
        self.assertEqual(result.verdict.exit_code, guard.EXIT_SAMPLE_FAILURE)
        self.assertEqual(killed, [100, 101])

    def test_failed_first_read_kills_the_root_only(self):
        result, killed, _ = self.run_watch([guard.SampleError("boom")])
        self.assertEqual(killed, [100])

    def test_a_recovered_read_resets_the_failure_count(self):
        config = guard.Config(max_sample_failures=2)
        script = [guard.SampleError("a"), sample(1), guard.SampleError("b"), sample(1), guard.ProcessGone(100)]
        result, killed, _ = self.run_watch(script, config)
        self.assertEqual(result.verdict.exit_code, guard.EXIT_GONE)
        self.assertEqual(killed, [])

    def test_a_child_that_already_exited_does_not_stop_the_kill(self):
        killed = []

        def kill(pid):
            if pid == 101:
                raise ProcessLookupError(pid)
            killed.append(pid)

        clock = FakeClock()
        result = guard.watch(
            100,
            guard.Config(),
            lambda root: sample(30, pids=(100, 101, 102)),
            kill,
            clock=clock,
            sleep=clock.sleep,
        )
        self.assertEqual(killed, [100, 102])
        self.assertEqual(result.killed, [100, 102])

    def test_records_the_largest_gap_between_samples(self):
        clock = FakeClock()
        steps = iter([sample(1), sample(1), sample(1), guard.ProcessGone(1)])
        reads = []

        def sample_fn(root):
            step = next(steps)
            if isinstance(step, Exception):
                raise step
            reads.append(1)
            if len(reads) == 2:
                clock.now += 0.25  # the second read is slow, so the next gap is 0.35 s
            return step

        result = guard.watch(
            100, guard.Config(), sample_fn, lambda pid: None, clock=clock, sleep=clock.sleep
        )
        self.assertAlmostEqual(result.max_gap_s, 0.35, places=3)

    def test_invalid_config_is_refused_before_any_sample(self):
        calls = []
        with self.assertRaises(guard.Refused):
            guard.watch(
                100,
                guard.Config(limit_bytes=21 * GIB),
                lambda root: calls.append(root),
                lambda pid: None,
            )
        self.assertEqual(calls, [])


class RefusePidTests(unittest.TestCase):
    def test_refuses_init_and_the_guard_itself(self):
        import os

        for pid in (0, 1, os.getpid()):
            with self.assertRaises(guard.Refused):
                guard._refuse_pid(pid, os.getpid(), [os.getppid()])
        with self.assertRaises(guard.Refused):
            guard._refuse_pid(os.getppid(), os.getpid(), [os.getppid()])


@unittest.skipUnless(sys.platform == "darwin", "needs macOS proc_pid_rusage")
class DarwinIntegrationTests(unittest.TestCase):
    def start_child(self, megabytes):
        code = textwrap.dedent(
            f"""
            import time
            block = bytearray({megabytes} * 1024 * 1024)
            for i in range(0, len(block), 4096):
                block[i] = 1
            time.sleep(30)
            """
        )
        return subprocess.Popen([sys.executable, "-I", "-c", code])

    def test_reads_footprint_and_kills_a_child_over_a_small_limit(self):
        child = self.start_child(300)
        try:
            sampler = guard.DarwinSampler()
            deadline = time.monotonic() + 20
            first = None
            while time.monotonic() < deadline:
                first = sampler(child.pid)
                if first.footprint > 200 * 1024 ** 2:
                    break
                time.sleep(0.05)
            self.assertIsNotNone(first)
            self.assertGreater(first.footprint, 200 * 1024 ** 2)
            result = guard.watch(
                child.pid,
                guard.Config(limit_bytes=128 * 1024 ** 2, deadline_s=20),
                sampler,
                lambda pid: __import__("os").kill(pid, 9),
            )
            self.assertEqual(result.verdict.exit_code, guard.EXIT_OVER_LIMIT)
            child.wait(timeout=5)
            self.assertEqual(child.returncode, -9)
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()

    def test_a_missing_pid_reads_as_gone(self):
        child = subprocess.Popen([sys.executable, "-I", "-c", "pass"])
        child.wait()
        sampler = guard.DarwinSampler()
        with self.assertRaises(guard.ProcessGone):
            sampler(child.pid)


if __name__ == "__main__":
    unittest.main()


class AncestorTests(unittest.TestCase):
    """The guard must never kill itself or a process it runs under."""

    CHAIN = {100: 50, 50: 10, 10: 1, 1: 0}

    def test_ancestor_chain_walks_up_to_init(self):
        self.assertEqual(guard.ancestors_of(100, self.CHAIN.get), [50, 10, 1])

    def test_ancestor_chain_stops_on_a_cycle_or_a_missing_parent(self):
        self.assertEqual(guard.ancestors_of(1, {1: 2, 2: 1}.get), [2])
        self.assertEqual(guard.ancestors_of(5, {}.get), [])

    def test_a_grandparent_is_refused_as_root(self):
        ancestors = guard.ancestors_of(100, self.CHAIN.get)
        for pid in (50, 10, 1, 100):
            with self.assertRaises(guard.Refused, msg=str(pid)):
                guard._refuse_pid(pid, 100, ancestors)
        guard._refuse_pid(777, 100, ancestors)  # an unrelated process is allowed

    def test_the_guards_own_pid_is_never_killed_even_if_it_is_in_the_tree(self):
        killed = []
        clock = FakeClock()
        guard.watch(
            100,
            guard.Config(),
            lambda root: sample(30, pids=(100, 555, 101)),
            killed.append,
            clock=clock,
            sleep=clock.sleep,
            protected=frozenset({555}),
        )
        self.assertEqual(killed, [100, 101])

    @unittest.skipUnless(sys.platform == "darwin", "needs macOS")
    def test_cli_refuses_a_real_grandparent(self):
        import os

        # sh is the guard's parent. This test process is its grandparent.
        result = subprocess.run(
            ["sh", "-c", f'"{sys.executable}" -I "{SCRIPT}" --pid {os.getpid()}; exit $?'],
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, guard.EXIT_REFUSED, result.stderr)
        self.assertIn("ancestor", result.stderr)


class FailureHandlingTests(unittest.TestCase):
    """A failed log write or a failed kill must not stop the guard from killing."""

    def run_over_limit(self, kill, log=None, pids=(100, 101, 102)):
        clock = FakeClock()
        messages = []
        result = guard.watch(
            100,
            guard.Config(),
            lambda root: sample(30, pids=pids),
            kill,
            clock=clock,
            sleep=clock.sleep,
            log=log,
            report=messages.append,
        )
        return result, messages

    def test_a_permission_error_on_one_pid_does_not_stop_the_others(self):
        killed = []

        def kill(pid):
            if pid == 100:
                raise PermissionError(1, "Operation not permitted")
            killed.append(pid)

        result, messages = self.run_over_limit(kill)
        self.assertEqual(killed, [101, 102])
        self.assertEqual(result.killed, [101, 102])
        self.assertEqual(len(result.kill_errors), 1)
        self.assertIn("100", result.kill_errors[0])
        self.assertTrue(any("could not kill 100" in message for message in messages))

    def test_a_log_write_error_does_not_stop_the_kill(self):
        killed = []

        def full_disk(row):
            raise OSError(28, "No space left on device")

        result, messages = self.run_over_limit(killed.append, log=full_disk)
        self.assertEqual(killed, [100, 101, 102])
        self.assertEqual(result.verdict.exit_code, guard.EXIT_OVER_LIMIT)
        self.assertTrue(any("log write failed" in message for message in messages))

    def test_a_log_error_is_reported_once_and_watching_continues(self):
        clock = FakeClock()
        steps = iter([sample(1), sample(2), sample(3), guard.ProcessGone(100)])
        calls = []

        def sample_fn(root):
            step = next(steps)
            if isinstance(step, Exception):
                raise step
            return step

        def bad_log(row):
            calls.append(row)
            raise OSError(5, "I/O error")

        messages = []
        result = guard.watch(
            100, guard.Config(), sample_fn, lambda pid: None,
            clock=clock, sleep=clock.sleep, log=bad_log, report=messages.append,
        )
        self.assertEqual(result.samples, 3)
        self.assertEqual(len(calls), 1)  # logging is switched off after the first failure
        self.assertEqual(len([m for m in messages if "log write failed" in m]), 1)

    @unittest.skipUnless(sys.platform == "darwin", "needs macOS")
    def test_cli_exits_6_when_a_process_could_not_be_killed(self):
        from unittest import mock

        verdict = guard.Verdict("kill", "footprint over", guard.EXIT_OVER_LIMIT)
        result = guard.Result(verdict, 1, 0, 0, 0.0, 0.0, [], ["could not kill 4242: denied"])
        with mock.patch.object(guard, "watch", return_value=result):
            self.assertEqual(guard.main(["--pid", "99999"]), guard.EXIT_KILL_FAILED)

    def test_cli_refuses_a_log_path_that_cannot_be_opened(self):
        code = guard.main(["--pid", "99999", "--log", "/nonexistent-dir-for-guard-test/guard.jsonl"])
        self.assertEqual(code, guard.EXIT_REFUSED)


class NonFiniteInputTests(unittest.TestCase):
    def test_non_finite_or_non_positive_limits_are_refused_with_a_message(self):
        for text in ("nan", "inf", "-inf", "0", "-1"):
            with self.assertRaises(guard.Refused, msg=text):
                guard.gib_to_bytes(float(text))

    def test_cli_refuses_nan_without_a_traceback(self):
        result = subprocess.run(
            [sys.executable, "-I", str(SCRIPT), "--pid", "99999", "--limit-gib", "nan"],
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, guard.EXIT_REFUSED)
        self.assertNotIn("Traceback", result.stderr)
        self.assertIn("refused", result.stderr)

    def test_a_nan_deadline_is_refused(self):
        with self.assertRaises(guard.Refused):
            guard.Config(deadline_s=float("nan")).validate()
        with self.assertRaises(guard.Refused):
            guard.Config(deadline_s=float("inf")).validate()

    def test_nan_interval_is_refused(self):
        with self.assertRaises(guard.Refused):
            guard.Config(interval_s=float("nan")).validate()
