#!/usr/bin/env python3
"""Measure a Claude Code game-building run; keep diagnostics and fail closed.

    scripts/agent-benchmark/run.py --naive-bin target/release [--budget 10]

Uses your Claude account and may open engine windows. A successful exit is a
completed smoke run, not independent acceptance of the Orb Harvest specification.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
TAIL_BYTES = 4096
MAX_RESULT_BYTES = 4 * 1024 * 1024
AUTHORING_EXTENSIONS = {"scenes": {".yaml", ".yml"}, "logic": {".lua"}, "tests": {".lua"}}


def tail(path, limit=TAIL_BYTES):
    """Read a bounded UTF-8 diagnostic tail, including binary timeout output."""
    with path.open("rb") as stream:
        stream.seek(max(0, path.stat().st_size - limit))
        return stream.read(limit).decode("utf-8", errors="replace")


def run_command(name, command, cwd, env, timeout, work):
    """Spool complete logs to disk without retaining subprocess output in memory."""
    stdout, stderr = work / f"{name}.stdout.log", work / f"{name}.stderr.log"
    record = {"status": "launch_error", "returncode": None, "timed_out": False,
              "stdout": str(stdout), "stderr": str(stderr), "error": None}
    started = time.monotonic()
    process = None
    with stdout.open("wb") as out, stderr.open("wb") as err:
        try:
            process = subprocess.Popen(command, cwd=cwd, env=env, stdout=out, stderr=err,
                                       start_new_session=True)
            try:
                process.wait(timeout=timeout)
                record["status"] = "completed"
            except subprocess.TimeoutExpired:
                record.update(status="timeout", timed_out=True)
        except OSError as error:
            record["error"] = f"{type(error).__name__}: {error}"
        finally:
            if process is not None:
                # Includes engines left behind by this agent. Never match arbitrary
                # process command lines with pkill, even using a unique socket path.
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                except OSError as error:
                    record["error"] = f"process-group cleanup failed: {error}"
                    if not record["timed_out"]:
                        record["status"] = "cleanup_error"
                    process.kill()
                process.wait()
                record["returncode"] = process.returncode
    record.update(wall_seconds=round(time.monotonic() - started, 3),
                  stdout_bytes=stdout.stat().st_size, stderr_bytes=stderr.stat().st_size,
                  stdout_tail=tail(stdout), stderr_tail=tail(stderr))
    return record


def command_succeeded(record):
    return record["status"] == "completed" and record["returncode"] == 0


def read_result(path):
    """Validate the CLI result envelope; never infer success from absent output."""
    if path.stat().st_size > MAX_RESULT_BYTES:
        raise ValueError(f"agent JSON exceeds {MAX_RESULT_BYTES} bytes; see complete stdout log")
    output = path.read_text(encoding="utf-8").strip()
    if not output:
        raise ValueError("agent produced no result")
    try:
        result = json.loads(output)
    except json.JSONDecodeError:
        result = json.loads(output.splitlines()[-1])
    if (not isinstance(result, dict) or result.get("type") != "result"
            or not isinstance(result.get("is_error"), bool)):
        raise ValueError("agent result must have type=result and boolean is_error")
    message = result.get("result", "")
    if not isinstance(message, str) or (not result["is_error"] and not message.strip()):
        raise ValueError("successful agent result must contain a nonempty result string")
    if not result["is_error"] and result.get("subtype", "success") != "success":
        raise ValueError(f"agent result subtype: {result['subtype']}")
    return result


def authoring_snapshot(project):
    """Hash eligible regular files; symlinks and empty files cannot prove authorship."""
    if project.is_symlink():
        raise OSError("project directory was replaced with a symlink")
    snapshot = {}
    for directory, extensions in AUTHORING_EXTENSIONS.items():
        if (project / directory).is_symlink():
            continue
        for path in sorted((project / directory).rglob("*")):
            if path.suffix not in extensions or not path.is_file() or path.is_symlink():
                continue
            digest = hashlib.sha256()
            size = 0
            with path.open("rb") as stream:
                for block in iter(lambda: stream.read(65536), b""):
                    digest.update(block)
                    size += len(block)
            snapshot[str(path.relative_to(project))] = {"sha256": digest.hexdigest(), "bytes": size}
    return snapshot


def authored_changes(baseline, current):
    added = sorted(current.keys() - baseline.keys())
    modified = sorted(path for path in current.keys() & baseline.keys()
                      if current[path]["sha256"] != baseline[path]["sha256"])
    changed = {Path(path).parts[0] for path in added + modified if current[path]["bytes"]}
    missing = sorted(AUTHORING_EXTENSIONS.keys() - changed)
    return {"sufficient": not missing, "added": added, "modified": modified,
            "deleted": sorted(baseline.keys() - current.keys()),
            "missing_changed_directories": missing,
            "baseline": baseline, "current": current}


def run_benchmark(args, work):
    bin_dir = Path(args.naive_bin).resolve()
    project = work / "orb_harvest"
    env = dict(os.environ, PATH=f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}")
    report = {"schema_version": 2, "success": False, "agent_success": False,
              "test_success": False, "failure_reasons": [], "project": str(project),
              "task_acceptance": {"status": "unverified", "reason":
                  "No independent Orb Harvest acceptance checks; agent-authored tests are smoke evidence."},
              "stages": {name: {"status": "not_run"} for name in ("init", "agent", "tests")}}
    result = {}
    baseline = None
    current = None
    for name in report["stages"]:
        # Artifacts exist even when an earlier stage prevents the command running.
        for stream in ("stdout", "stderr"):
            path = work / f"{name}.{stream}.log"
            path.touch()
            report["stages"][name][stream] = str(path)
    try:
        init = run_command("init", [str(bin_dir / "naive"), "init", "orb_harvest"],
                           work, env, args.test_timeout, work)
        report["stages"]["init"] = init
        if not command_succeeded(init):
            report["failure_reasons"].append("initialization_failed")
        else:
            baseline = authoring_snapshot(project)
            socket_path = work / "engine.sock"
            mcp_config = work / "mcp.json"
            mcp_config.write_text(json.dumps({"mcpServers": {"naive": {
                "command": str(bin_dir / "naive_mcp"), "args": [str(socket_path)]}}}))
            prompt = ((HERE / "prompt.md").read_text()
                      .replace("NAIVE_GUIDE", str(REPO / "docs" / "GAME_DEVELOPMENT_GUIDE.md"))
                      .replace("NAIVE_SOCKET", str(socket_path)))
            command = ["claude", "-p", prompt, "--output-format", "json",
                       "--max-budget-usd", str(args.budget), "--mcp-config", str(mcp_config),
                       "--permission-mode", "acceptEdits", "--allowedTools", "Bash", "Read",
                       "Write", "Edit", "Glob", "Grep", "mcp__naive", "--add-dir", str(REPO / "docs")]
            if args.model:
                command += ["--model", args.model]
            agent = run_command("agent", command, project, env, args.timeout, work)
            report["stages"]["agent"] = agent
            if not command_succeeded(agent):
                report["failure_reasons"].append(f"agent_{agent['status']}" if agent["status"] != "completed"
                                                 else "agent_exit_nonzero")
            try:
                result = read_result(Path(agent["stdout"]))
            except (ValueError, OSError) as error:
                agent["result_error"] = str(error)
                report["failure_reasons"].append("agent_invalid_result")
            if result.get("is_error"):
                report["failure_reasons"].append("agent_reported_error")
            report["agent_success"] = (command_succeeded(agent) and bool(result)
                                       and result.get("is_error") is False)
            current = authoring_snapshot(project)
            # Test diagnostics are useful even after an unsuccessful agent run.
            tests = run_command("tests", [str(bin_dir / "naive"), "test"], project, env,
                                args.test_timeout, work)
            report["stages"]["tests"] = tests
            report["test_success"] = command_succeeded(tests)
            if not report["test_success"]:
                report["failure_reasons"].append("tests_failed")
    except Exception as error:
        report["failure_reasons"].append("harness_error")
        report["harness_error"] = f"{type(error).__name__}: {error}"
    finally:
        if baseline is not None:
            try:
                changes = authored_changes(baseline, current if current is not None
                                           else authoring_snapshot(project))
                report["authored_changes"] = changes
                report["files"] = {directory: sum(Path(path).parts[0] == directory
                                                  for path in changes["current"])
                                   for directory in AUTHORING_EXTENSIONS}
                if not changes["sufficient"]:
                    report["failure_reasons"].append("missing_authored_changes")
            except OSError as error:
                report["failure_reasons"].append("project_inspection_failed")
                report["inspection_error"] = str(error)
        usage = result.get("usage") if isinstance(result.get("usage"), dict) else {}
        model_usage = result.get("modelUsage")
        agent = report["stages"]["agent"]
        tests = report["stages"]["tests"]
        report.update(wall_minutes=round(agent.get("wall_seconds", 0) / 60, 1),
                      agent_reported_ms=result.get("duration_ms"), turns=result.get("num_turns"),
                      cost_usd=result.get("total_cost_usd"), input_tokens=usage.get("input_tokens"),
                      cache_read_tokens=usage.get("cache_read_input_tokens"),
                      cache_write_tokens=usage.get("cache_creation_input_tokens"),
                      output_tokens=usage.get("output_tokens"),
                      models=list(model_usage) if isinstance(model_usage, dict) else [],
                      agent_error=result.get("is_error"), naive_test_exit=tests.get("returncode"),
                      naive_test_summary=next((line for line in tests.get("stdout_tail", "").splitlines()[::-1]
                                               if "passed" in line), ""),
                      final_message=result.get("result", "")[-1500:])
        report["success"] = (report["agent_success"] and report["test_success"]
                             and report.get("authored_changes", {}).get("sufficient", False)
                             and not report["failure_reasons"])
        (work / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


def positive_number(value):
    number = float(value)
    if not 0 < number < float("inf"):
        raise argparse.ArgumentTypeError("must be a finite positive number")
    return number


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--naive-bin", required=True, help="directory with naive and naive_mcp")
    parser.add_argument("--budget", type=positive_number, default=10.0, help="max USD for the agent")
    parser.add_argument("--timeout", type=positive_number, default=2400, help="agent timeout in seconds")
    parser.add_argument("--test-timeout", type=positive_number, default=300,
                        help="timeout in seconds for initialization and naive test")
    parser.add_argument("--model", help="model for the agent (default: the CLI's)")
    args = parser.parse_args(argv)
    work = Path(tempfile.mkdtemp(prefix="naive-agent-bench-"))
    print(f"Project: {work / 'orb_harvest'}", flush=True)
    report = run_benchmark(args, work)
    print(json.dumps(report, indent=2))
    print(f"Report and complete stdout/stderr logs in {work}; the project is kept.")
    return 0 if report["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
