#!/usr/bin/env python3
"""Tag Codex shell commands and clean up their children at the session boundary.

No daemon, PID registry, command-name matching, or transcript parsing. The marker
is inherited by children, including children orphaned when their launcher exits.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time


OWNER_ENV = "HULLA_CODEX_PROCESS_OWNER"
REPO = Path(__file__).resolve().parent.parent


def marker(session_id):
    return hashlib.sha256((str(REPO) + "\0" + session_id).encode()).hexdigest()


def owned_processes(owner, pid=None):
    # Never print this output: ps includes environments, which may hold secrets.
    command = ["ps", "eww"]
    command += ["-p", str(pid)] if pid is not None else ["-A"]
    command += ["-o", "pid=,uid=,lstart=,stat=,command="]
    result = subprocess.run(command, capture_output=True, text=True, timeout=0.5)
    if result.returncode and not (pid is not None and result.returncode == 1):
        raise RuntimeError("process inspection failed; cleanup is unverified")
    pattern = re.compile(r"(?:^|\s)" + OWNER_ENV + "=" + owner + r"(?:\s|$)")
    processes = {}
    for line in result.stdout.splitlines():
        fields = line.split(None, 8)
        if len(fields) != 9:
            continue
        process_id, user_id = int(fields[0]), int(fields[1])
        if (process_id != os.getpid() and user_id == os.getuid()
                and "Z" not in fields[7] and pattern.search(fields[8])):
            processes[process_id] = " ".join(fields[2:7])
    return processes


def signal_owned(owner, processes, sig, deadline):
    for pid, started in processes.items():
        if time.monotonic() >= deadline:
            break
        # Recheck both ownership and start time before signaling a saved PID.
        if owned_processes(owner, pid).get(pid) != started:
            continue
        try:
            os.kill(pid, sig)
        except ProcessLookupError:
            pass


def cleanup(owner):
    deadline = time.monotonic() + 1.8
    processes = owned_processes(owner)
    if not processes:
        return 0, {}
    count = len(processes)
    signal_owned(owner, processes, signal.SIGTERM, deadline)
    time.sleep(0.25)
    remaining = owned_processes(owner)
    if remaining:
        signal_owned(owner, remaining, signal.SIGKILL, deadline)
        time.sleep(0.05)
        remaining = owned_processes(owner)
    return count, remaining


def failure(event, reason):
    output = {"systemMessage": "Codex process cleanup: " + reason}
    if event.get("hook_event_name") == "PreToolUse":
        output["hookSpecificOutput"] = {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    elif (event.get("hook_event_name") == "Stop"
          and not event.get("stop_hook_active")):
        # Ask for corrective action once; never trap the agent in a stop loop.
        output.update(decision="block", reason=reason)
    return output


def handle(event):
    session_id = event.get("session_id")
    if not isinstance(session_id, str) or not session_id:
        return failure(event, "No session id; process ownership cannot be verified.")
    owner = marker(session_id)
    name = event.get("hook_event_name")
    if name == "PreToolUse":
        tool_input = event.get("tool_input", {})
        command = tool_input.get("command")
        if not isinstance(command, str):
            return failure(event, "Expected a shell command; ownership tag was not applied.")
        updated = dict(tool_input)
        updated["command"] = f"export {OWNER_ENV}={owner}\n" + command
        return {"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": updated,
        }}
    if name in ("Stop", "Interrupt", "SessionEnd"):
        count, remaining = cleanup(owner)
        if remaining:
            return failure(event, "Owned processes still running: "
                           + ", ".join(map(str, remaining)) + ". Verify cleanup.")
        if count:
            return {"systemMessage": f"Cleaned up {count} leftover Codex process(es)."}
    return {}


def main():
    event = json.load(sys.stdin)
    try:
        output = handle(event)
    except (OSError, RuntimeError, subprocess.TimeoutExpired):
        # Do not expose command lines or environment contents on error.
        output = failure(event, "Process cleanup failed; termination is unverified.")
    print(json.dumps(output))


if __name__ == "__main__":
    main()
