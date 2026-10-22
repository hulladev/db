# Codex process cleanup

The repo-local `.codex/hooks.json` tags shell commands before execution and
cleans up this Codex session's remaining tagged processes on `Stop`, `Interrupt`
and `SessionEnd`. It sends TERM, waits briefly, then sends KILL if necessary and
checks the result. Process environments and command lines are never logged.
There is no daemon, supervisor, process registry or production dependency.

Codex requires review of new hook definitions before they run. From the Codex CLI
in this repo, use `/hooks` to review and trust the definitions. Changed definitions
require review again. See the [official hook documentation](https://learn.chatgpt.com/docs/hooks).

Ownership comes from a session-specific environment marker inherited by shell
children. The cleanup hook checks the marker, user and process start time before
signaling a PID. Other chats, Claude, and user-started processes are not matched
by their executable names or working directory. All shell children of this
session are considered temporary, so start services intended to outlive a turn
from your own terminal. Do not remove the ownership marker in agent commands.

Subagents share the parent session ID. Their tagged processes are cleaned at the
parent boundary; `SubagentStop` deliberately does not kill processes because
that would interrupt other agents still working in the same session.

This is a best-effort guardrail. It cannot run after an application crash, track
processes launched through tools that bypass shell hooks, or identify children
that discard their environment. macOS also hides environments for some protected
system executables. Bun and Python child/orphan detection are verified locally.
If inspection fails, the hook reports cleanup as unverified and asks the agent
to fix it once, without creating an endless continuation loop.

Run the focused tests with permission to inspect processes:

```sh
python3 -m unittest discover -s scripts/tests -p test_codex_process_cleanup.py -v
```
