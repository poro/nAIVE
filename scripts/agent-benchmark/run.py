#!/usr/bin/env python3
"""Measure an agent building a small game in nAIVE from a prompt.

Creates a fresh project with `naive init`, gives Claude Code (headless, `claude
-p`) the prompt in prompt.md with the `naive` CLI and nAIVE's MCP tools, then
checks the result itself: runs `naive test` and counts the project's files.
Prints and writes a JSON report: wall time, turns, tokens, cost, test results.

    scripts/agent-benchmark/run.py --naive-bin target/release [--budget 10]

Runs on your Claude account and opens engine windows while it works.
"""

import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--naive-bin", required=True, help="directory with naive and naive_mcp")
    parser.add_argument("--budget", type=float, default=10.0, help="max USD for the agent")
    parser.add_argument("--timeout", type=int, default=2400, help="seconds before giving up")
    parser.add_argument("--model", help="model for the agent (default: the CLI's)")
    args = parser.parse_args()
    bin_dir = Path(args.naive_bin).resolve()

    work = Path(tempfile.mkdtemp(prefix="naive-agent-bench-"))
    env = dict(os.environ, PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}")
    subprocess.run(["naive", "init", "orb_harvest"], cwd=work, env=env, check=True,
                   capture_output=True)
    project = work / "orb_harvest"
    socket_path = work / "engine.sock"
    mcp_config = work / "mcp.json"
    mcp_config.write_text(json.dumps({"mcpServers": {"naive": {
        "command": str(bin_dir / "naive_mcp"), "args": [str(socket_path)]}}}))
    prompt = ((HERE / "prompt.md").read_text()
              .replace("NAIVE_GUIDE", str(REPO / "docs" / "GAME_DEVELOPMENT_GUIDE.md"))
              .replace("NAIVE_SOCKET", str(socket_path)))

    command = ["claude", "-p", prompt, "--output-format", "json",
               "--max-budget-usd", str(args.budget),
               "--mcp-config", str(mcp_config),
               "--permission-mode", "acceptEdits",
               "--allowedTools", "Bash", "Read", "Write", "Edit", "Glob", "Grep", "mcp__naive",
               "--add-dir", str(REPO / "docs")]
    if args.model:
        command += ["--model", args.model]
    print(f"Project: {project}", flush=True)
    started = time.monotonic()
    try:
        agent = subprocess.run(command, cwd=project, env=env, capture_output=True, text=True,
                               timeout=args.timeout)
        output = agent.stdout
    except subprocess.TimeoutExpired as expired:
        output = expired.stdout or ""
    elapsed = time.monotonic() - started
    # Engines the agent left running.
    subprocess.run(["pkill", "-f", str(socket_path)], check=False)

    try:
        result = json.loads(output.strip().splitlines()[-1]) if output.strip() else {}
    except json.JSONDecodeError:
        result = {"raw": output[-2000:]}

    tests = subprocess.run(["naive", "test"], cwd=project, env=env, capture_output=True,
                           text=True, timeout=300)
    summary_line = next((l for l in tests.stdout.splitlines()[::-1] if "passed" in l), "")
    files = {d: len(list((project / d).rglob("*"))) for d in ["scenes", "logic", "tests"]
             if (project / d).exists()}
    usage = result.get("usage", {})
    report = {
        "wall_minutes": round(elapsed / 60, 1),
        "agent_reported_ms": result.get("duration_ms"),
        "turns": result.get("num_turns"),
        "cost_usd": result.get("total_cost_usd"),
        "input_tokens": usage.get("input_tokens"),
        "cache_read_tokens": usage.get("cache_read_input_tokens"),
        "cache_write_tokens": usage.get("cache_creation_input_tokens"),
        "output_tokens": usage.get("output_tokens"),
        "models": list((result.get("modelUsage") or {}).keys()),
        "agent_error": result.get("is_error"),
        "naive_test_exit": tests.returncode,
        "naive_test_summary": summary_line,
        "files": files,
        "project": str(project),
        "final_message": (result.get("result") or "")[-1500:],
    }
    print(json.dumps(report, indent=2))
    (work / "report.json").write_text(json.dumps(report, indent=2))
    print(f"Report also in {work / 'report.json'}; the project is kept at {project}")
    return 0 if tests.returncode == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
