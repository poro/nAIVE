"""Offline benchmark regressions: fake CLIs, no engine build or paid model calls."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("benchmark", Path(__file__).with_name("run.py"))
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)

FAKE_CLI = r'''
import json
import os
from pathlib import Path
import sys
import time

mode = os.environ.get("BENCH_TEST_MODE", "success")
if Path(sys.argv[0]).name == "naive":
    if sys.argv[1] == "init":
        print("init diagnostic", file=sys.stderr)
        if mode == "init_failure":
            sys.exit(7)
        root = Path("orb_harvest")
        for name, text in {"scenes/main.yaml": "entities: []\n",
                           "logic/main.lua": "-- starter logic\n",
                           "tests/starter.lua": "-- starter test\n"}.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        sys.exit(0)
    print("test diagnostic", file=sys.stderr, flush=True)
    if mode == "test_timeout":
        os.write(2, b"partial test stderr\xff\n")
        time.sleep(5)
    if mode == "tests_mutate":
        for name in ("scenes/main.yaml", "logic/main.lua", "tests/starter.lua"):
            Path(name).write_text("changed by test runner")
    print("1 passed, 0 failed", flush=True)
    sys.exit(9 if mode == "test_failure" else 0)

print("agent diagnostic", file=sys.stderr, flush=True)
if mode not in ("unchanged", "tests_mutate", "deletion_only", "noncode_only", "empty_only"):
    for name, text in {"scenes/main.yaml": "entities: [craft]\n",
                       "logic/game.lua": "score = 0\n",
                       "tests/game.lua": "assert(score == 0)\n"}.items():
        Path(name).write_text(text)
if mode == "deletion_only":
    for path in (Path("scenes/main.yaml"), Path("logic/main.lua"), Path("tests/starter.lua")):
        path.unlink()
if mode in ("noncode_only", "empty_only"):
    for directory in ("scenes", "logic", "tests"):
        Path(directory, "notes.txt" if mode == "noncode_only" else "empty.lua").write_text(
            "notes" if mode == "noncode_only" else "")
if mode == "agent_timeout":
    os.write(1, b"partial stdout\xff\n")
    os.write(2, b"partial stderr\xff\n")
    time.sleep(5)
if mode == "missing":
    sys.exit(0)
if mode == "invalid_json":
    print("not JSON")
    sys.exit(0)
if mode == "invalid_utf8":
    os.write(1, b'{"type":"result","is_error":false,"result":"bad\xff"}\n')
    sys.exit(0)
result = {"type": "result", "subtype": "success", "is_error": False,
          "result": "Built the game", "usage": {"input_tokens": 12}, "num_turns": 3}
if mode == "is_error":
    result = {"type": "result", "subtype": "error_max_budget_usd", "is_error": True}
if mode == "missing_result":
    del result["result"]
if mode == "invalid_envelope":
    result = []
if mode == "noisy":
    os.write(2, b"start of complete stderr\n" + b"x" * 10000)
print(json.dumps(result))
sys.exit(4 if mode == "agent_nonzero" else 0)
'''


class BenchmarkTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="naive-benchmark-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin_dir = self.root / "bin"
        self.bin_dir.mkdir()
        for name in ("naive", "claude"):
            executable = self.bin_dir / name
            executable.write_text(f"#!{sys.executable}\n" + FAKE_CLI)
            executable.chmod(0o755)
        self.args = SimpleNamespace(naive_bin=str(self.bin_dir), budget=1,
                                    timeout=3, test_timeout=3, model=None)
        self.run_number = 0

    def run_case(self, mode="success"):
        self.run_number += 1
        work = self.root / f"run-{self.run_number}"
        work.mkdir()
        with mock.patch.dict(os.environ, {"BENCH_TEST_MODE": mode}):
            report = benchmark.run_benchmark(self.args, work)
        self.assertEqual(report, json.loads((work / "report.json").read_text()))
        for stage in ("init", "agent", "tests"):
            for stream in ("stdout", "stderr"):
                self.assertTrue(Path(report["stages"][stage][stream]).exists())
        return report

    def test_completed_run_has_separate_smoke_and_acceptance_results(self):
        report = self.run_case()
        self.assertTrue(report["success"])
        self.assertTrue(report["agent_success"])
        self.assertTrue(report["test_success"])
        self.assertEqual(report["task_acceptance"]["status"], "unverified")
        self.assertEqual(report["input_tokens"], 12)
        changes = report["authored_changes"]
        self.assertEqual(changes["modified"], ["scenes/main.yaml"])
        self.assertEqual(changes["added"], ["logic/game.lua", "tests/game.lua"])
        self.assertNotEqual(changes["baseline"]["scenes/main.yaml"]["sha256"],
                            changes["current"]["scenes/main.yaml"]["sha256"])

    def test_agent_failure_cannot_be_hidden_by_passing_tests(self):
        for mode, reason in (("agent_nonzero", "agent_exit_nonzero"),
                             ("is_error", "agent_reported_error"),
                             ("missing", "agent_invalid_result"),
                             ("invalid_json", "agent_invalid_result"),
                             ("invalid_utf8", "agent_invalid_result"),
                             ("invalid_envelope", "agent_invalid_result"),
                             ("missing_result", "agent_invalid_result")):
            with self.subTest(mode=mode):
                report = self.run_case(mode)
                self.assertFalse(report["success"])
                self.assertFalse(report["agent_success"])
                self.assertTrue(report["test_success"])
                self.assertIn(reason, report["failure_reasons"])

    def test_untouched_and_non_material_files_do_not_count_as_authorship(self):
        for mode in ("unchanged", "deletion_only", "noncode_only", "empty_only", "tests_mutate"):
            with self.subTest(mode=mode):
                report = self.run_case(mode)
                self.assertTrue(report["agent_success"])
                self.assertTrue(report["test_success"])
                self.assertFalse(report["success"])
                self.assertIn("missing_authored_changes", report["failure_reasons"])
                self.assertEqual(report["authored_changes"]["missing_changed_directories"],
                                 ["logic", "scenes", "tests"])

    def test_timeout_keeps_binary_output_and_still_runs_tests(self):
        self.args.timeout = 1
        report = self.run_case("agent_timeout")
        self.assertFalse(report["success"])
        self.assertTrue(report["test_success"])
        self.assertIn("agent_timeout", report["failure_reasons"])
        agent = report["stages"]["agent"]
        self.assertTrue(agent["timed_out"])
        self.assertIn("partial stderr\ufffd", agent["stderr_tail"])
        self.assertIn(b"partial stdout\xff", Path(agent["stdout"]).read_bytes())

    def test_test_failures_and_timeouts_write_reports(self):
        for mode in ("test_failure", "test_timeout"):
            with self.subTest(mode=mode):
                self.args.test_timeout = 1 if mode == "test_timeout" else 3
                report = self.run_case(mode)
                self.assertFalse(report["success"])
                self.assertTrue(report["agent_success"])
                self.assertFalse(report["test_success"])
                self.assertIn("tests_failed", report["failure_reasons"])
                self.assertIn("test diagnostic", report["stages"]["tests"]["stderr_tail"])
                self.assertEqual(report["stages"]["tests"]["timed_out"], mode == "test_timeout")

    def test_initialization_failure_keeps_diagnostics_and_skips_agent(self):
        report = self.run_case("init_failure")
        self.assertFalse(report["success"])
        self.assertEqual(report["failure_reasons"], ["initialization_failed"])
        self.assertEqual(report["stages"]["init"]["returncode"], 7)
        self.assertIn("init diagnostic", report["stages"]["init"]["stderr_tail"])
        self.assertEqual(report["stages"]["agent"]["status"], "not_run")

    def test_launch_errors_at_every_stage_keep_reports(self):
        popen = subprocess.Popen
        for failed_stage in ("init", "agent", "tests"):
            def launch(command, **kwargs):
                stage = "agent" if command[0] == "claude" else (
                    "init" if command[1] == "init" else "tests")
                if stage == failed_stage:
                    raise FileNotFoundError(f"missing {failed_stage} executable")
                return popen(command, **kwargs)
            with self.subTest(stage=failed_stage), mock.patch.object(benchmark.subprocess, "Popen", launch):
                report = self.run_case()
                self.assertFalse(report["success"])
                record = report["stages"][failed_stage]
                self.assertEqual(record["status"], "launch_error")
                self.assertIn(f"missing {failed_stage}", record["error"])

    def test_diagnostic_tails_are_bounded_and_logs_are_complete(self):
        report = self.run_case("noisy")
        agent = report["stages"]["agent"]
        self.assertTrue(report["success"])
        self.assertEqual(len(agent["stderr_tail"]), benchmark.TAIL_BYTES)
        complete = Path(agent["stderr"]).read_bytes()
        self.assertIn(b"start of complete stderr", complete)
        self.assertEqual(agent["stderr_bytes"], len(complete))

    def test_oversize_json_fails_closed_and_keeps_full_log(self):
        with mock.patch.object(benchmark, "MAX_RESULT_BYTES", 16):
            report = self.run_case()
        self.assertFalse(report["success"])
        self.assertIn("agent_invalid_result", report["failure_reasons"])
        self.assertGreater(Path(report["stages"]["agent"]["stdout"]).stat().st_size, 16)

    def test_result_parser_accepts_pretty_json_and_rejects_unsuccessful_subtype(self):
        path = self.root / "result.json"
        result = {"type": "result", "is_error": False, "result": "Complete"}
        path.write_text(json.dumps(result, indent=2))
        self.assertEqual(benchmark.read_result(path), result)
        result["subtype"] = "error_max_turns"
        path.write_text(json.dumps(result))
        with self.assertRaises(ValueError):
            benchmark.read_result(path)

    def test_cleanup_targets_only_each_launched_process_group(self):
        with mock.patch.object(benchmark.os, "killpg", wraps=os.killpg) as cleanup:
            report = self.run_case()
        self.assertTrue(report["success"])
        self.assertEqual(cleanup.call_count, 3)
        for call in cleanup.call_args_list:
            self.assertNotEqual(call.args[0], os.getpgrp())
            self.assertEqual(call.args[1], benchmark.signal.SIGKILL)

    def test_cli_exit_status_matches_report(self):
        for mode, exit_code in (("success", 0), ("agent_nonzero", 1), ("unchanged", 1)):
            self.run_number += 1
            work = self.root / f"cli-{self.run_number}"
            work.mkdir()
            with self.subTest(mode=mode), mock.patch.dict(os.environ, {"BENCH_TEST_MODE": mode}), \
                    mock.patch.object(benchmark.tempfile, "mkdtemp", return_value=str(work)), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(benchmark.main(["--naive-bin", str(self.bin_dir)]), exit_code)


if __name__ == "__main__":
    unittest.main()
