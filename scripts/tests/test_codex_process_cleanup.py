"""Real-process regressions for scoped cleanup; requires permission to run ps."""

import importlib.util
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import time
import unittest
import uuid


SCRIPT = Path(__file__).resolve().parents[1] / "codex_process_cleanup.py"
spec = importlib.util.spec_from_file_location("cleanup", SCRIPT)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)


class CleanupTests(unittest.TestCase):
    def event(self, name, session, **extra):
        return {"hook_event_name": name, "session_id": session, **extra}

    def call_hook(self, event):
        root = SCRIPT.parent.parent
        config = json.loads((root / ".codex/hooks.json").read_text())
        command = config["hooks"][event["hook_event_name"]][0]["hooks"][0]["command"]
        response = subprocess.run(["/bin/sh", "-c", command],
                                  cwd=root / "packages/db", input=json.dumps(event),
                                  capture_output=True, text=True, check=True, timeout=3)
        return json.loads(response.stdout)

    def test_preserves_shell_options_and_command_semantics(self):
        session = uuid.uuid4().hex
        command = "printf '%s' 'literal $HOME `unchanged`'"
        output = self.call_hook(self.event("PreToolUse", session, tool_input={
            "command": command, "timeout_ms": 1000,
        }))["hookSpecificOutput"]["updatedInput"]
        self.assertEqual(output["timeout_ms"], 1000)
        result = subprocess.run(["/bin/sh", "-c", output["command"]],
                                capture_output=True, text=True, check=True)
        self.assertEqual(result.stdout, "literal $HOME `unchanged`")

    def test_cleans_orphans_and_term_resistant_children_without_cross_session_kills(self):
        session, other = uuid.uuid4().hex, uuid.uuid4().hex
        children = []
        try:
            for identity in (session, other):
                env = dict(os.environ, **{hook.OWNER_ENV: hook.marker(identity)})
                child = subprocess.Popen([
                    sys.executable, "-c",
                    "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                    "print('ready',flush=True); time.sleep(60)",
                ], env=env, stdout=subprocess.PIPE, text=True)
                children.append(child)
                self.assertEqual(child.stdout.readline().strip(), "ready")

            # Launch a grandchild whose shell exits, reproducing the orphaned tsc.
            pre = hook.handle(self.event("PreToolUse", session, tool_input={
                "command": shlex.join([sys.executable, "-c", "import time; time.sleep(60)"])
                + " </dev/null >/dev/null 2>&1 & echo $!",
            }))
            launched = subprocess.run([
                "/bin/sh", "-c", pre["hookSpecificOutput"]["updatedInput"]["command"],
            ], capture_output=True, text=True, check=True)
            orphan = int(launched.stdout.strip())
            self.addCleanup(self.kill_if_alive, orphan)
            deadline = time.monotonic() + 1
            while (orphan not in hook.owned_processes(hook.marker(session))
                   and time.monotonic() < deadline):
                time.sleep(0.01)
            self.assertIn(orphan, hook.owned_processes(hook.marker(session)))

            response = self.call_hook(self.event("Interrupt", session))
            self.assertIn("Cleaned up", response["systemMessage"])
            self.assertEqual(children[0].wait(timeout=1), -9)
            self.assertIsNone(children[1].poll())
            self.assertFalse(hook.owned_processes(hook.marker(session)))
            self.assertIn(children[1].pid, hook.owned_processes(hook.marker(other)))
            self.assertEqual(hook.handle(self.event("Stop", session)), {})
        finally:
            for child in children:
                if child.poll() is None:
                    child.kill()
                child.wait(timeout=2)
                child.stdout.close()

    @staticmethod
    def kill_if_alive(pid):
        try:
            os.kill(pid, 9)
        except ProcessLookupError:
            pass


if __name__ == "__main__":
    unittest.main()
