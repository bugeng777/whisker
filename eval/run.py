#!/usr/bin/env python3
"""Run the whisker eval fixtures against a real provider and write a report."""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path

EVAL_DIR = Path(__file__).resolve().parent
ROOT = EVAL_DIR.parent
BIN = ROOT / "target" / "release" / "whisker"
TASKS_FILE = EVAL_DIR / "tasks.toml"
FIXTURES = EVAL_DIR / "fixtures"
CONFIG = EVAL_DIR / "eval-config.toml"


def load_tasks():
    with open(TASKS_FILE, "rb") as handle:
        data = tomllib.load(handle)
    return data.get("task", [])


def session_metrics(home: Path):
    sessions = sorted((home / ".whisker" / "sessions").glob("*/events.jsonl"))
    if not sessions:
        return {"session": None, "tool_calls": 0, "turns": 0, "tokens_in": 0,
                "tokens_out": 0, "reminders": 0, "todo_updates": 0, "errors": []}
    path = sessions[-1]
    events = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if line:
            try:
                events.append(json.loads(line))
            except json.JSONDecodeError:
                pass
    turns = [event.get("turn", 0) for event in events if event.get("type") == "turn_finished"]
    return {
        "session": str(path.parent),
        "tool_calls": sum(1 for event in events if event.get("type") == "tool_start"),
        "turns": max(turns, default=0),
        "tokens_in": sum(event.get("prompt_tokens", 0) for event in events if event.get("type") == "turn_finished"),
        "tokens_out": sum(event.get("completion_tokens", 0) for event in events if event.get("type") == "turn_finished"),
        "reminders": sum(1 for event in events if event.get("type") == "reminder"),
        "todo_updates": sum(1 for event in events if event.get("type") == "todo_updated"),
        "errors": [event.get("message", "") for event in events if event.get("type") == "error"],
    }


def run_task(task, keep=False):
    work = Path(tempfile.mkdtemp(prefix=f"whisker-eval-{task['name']}-"))
    fixture = FIXTURES / task["fixture"]
    shutil.copytree(fixture, work, dirs_exist_ok=True)
    home = work / ".home"
    home.mkdir()

    env = os.environ.copy()
    env["HOME"] = str(home)
    env["WHISKER_ENV_FILE"] = str(ROOT / ".env")
    env["NO_COLOR"] = "1"

    prompt = task["prompt"]
    timeout = int(task.get("timeout", 300))
    started = time.time()
    try:
        run = subprocess.run(
            [str(BIN), "-p", prompt, "--auto", "--cwd", str(work), "--config", str(CONFIG)],
            env=env, cwd=work, capture_output=True, text=True, timeout=timeout,
        )
        run_output = (run.stdout or "") + (run.stderr or "")
        timed_out = False
    except subprocess.TimeoutExpired as exc:
        run_output = (exc.stdout or "") + (exc.stderr or "")
        timed_out = True

    duration = time.time() - started
    verify = subprocess.run(
        ["bash", "-lc", task["verify"]], cwd=work, capture_output=True, text=True, timeout=120,
    )
    metrics = session_metrics(home)
    result = {
        "name": task["name"],
        "success": verify.returncode == 0 and not timed_out,
        "timed_out": timed_out,
        "duration_s": round(duration, 2),
        "verify_command": task["verify"],
        "verify_output": (verify.stdout + verify.stderr).strip()[:2000],
        "agent_output": run_output.strip()[-3000:],
        **metrics,
    }
    if not keep:
        shutil.rmtree(work, ignore_errors=True)
    else:
        result["workspace"] = str(work)
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true", help="validate tasks and fixtures only")
    parser.add_argument("--task", help="run a single task by name")
    parser.add_argument("--keep", action="store_true", help="keep temporary workspaces")
    args = parser.parse_args()

    tasks = load_tasks()
    if args.task:
        tasks = [task for task in tasks if task["name"] == args.task]
        if not tasks:
            print(f"unknown task: {args.task}", file=sys.stderr)
            return 1

    if args.dry_run:
        for task in tasks:
            missing = [key for key in ("name", "fixture", "prompt", "verify") if key not in task]
            if missing:
                print(f"{task.get('name', '?')}: missing {missing}", file=sys.stderr)
                return 1
            if not (FIXTURES / task["fixture"]).is_dir():
                print(f"{task['name']}: fixture not found", file=sys.stderr)
                return 1
            print(f"{task['name']}: ok")
        print(f"dry run: {len(tasks)} tasks valid")
        return 0

    if not BIN.exists():
        print(f"missing {BIN}; run: cargo build --release", file=sys.stderr)
        return 1
    if not (ROOT / ".env").exists() and not os.environ.get("DEEPSEEK_API_KEY"):
        print("no API key: fill agent/.env or export DEEPSEEK_API_KEY", file=sys.stderr)
        return 1

    results = []
    for task in tasks:
        print(f"running {task['name']} ...", flush=True)
        results.append(run_task(task, keep=args.keep))

    print()
    print(f"{'task':<22} {'result':<7} {'time':>7} {'tools':>6} {'turns':>6} {'in':>7} {'out':>6}")
    for result in results:
        status = "PASS" if result["success"] else "FAIL"
        print(
            f"{result['name']:<22} {status:<7} {result['duration_s']:>6.1f}s "
            f"{result['tool_calls']:>6} {result['turns']:>6} "
            f"{result['tokens_in']:>7} {result['tokens_out']:>6}"
        )

    passed = sum(1 for result in results if result["success"])
    report = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "passed": passed,
        "total": len(results),
        "results": results,
    }
    report_path = EVAL_DIR / "report.json"
    report_path.write_text(json.dumps(report, indent=2, ensure_ascii=False))
    print(f"\n{passed}/{len(results)} passed · report: {report_path}")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
