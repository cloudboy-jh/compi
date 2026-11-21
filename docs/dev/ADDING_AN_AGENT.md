# Adding an agent

Compi chats with installed agent programs (OMP, Claude Code, Codex). An agent is one entry and one driver; everything else (the launcher, the shell soup, Settings, the palette, the model picker, `compi chat --agent`, project settings, the smoke test) reads the entry. This is the recipe Codex was added from.

## How an agent fits

- **Entry** (`crates/compi-protocol/src/chat.rs`): a `ChatHarness` variant and its `AgentEntry` in `ENTRIES`: name, program (the `--agent` and `[chat.<program>]` word), palette keywords, mark, whether bare model IDs are its own, what its permission modes are called (`approval_modes` for Compi's Always ask / Allow edits / Full access, `other_modes` for any other mode it can report), where its real program is (`linux_folders`, `windows`) and the variable that sets its Windows build's Bash (`windows_bash`). The client and the daemon read nothing else about an agent.
- **Driver** (`crates/compi-daemon/src/chat/<agent>.rs`): turns the agent's wire protocol into the chat core's calls. It exposes `pub(super) struct <Agent>` (its state, in `Driver::<Agent>`) and `pub(super) const WIRE: Wire`, the functions the core calls: launch arguments, start, one message, prompt, cancel/interrupt, permission and question answers, setting changes, the session list, resume. `Driver::new` and `Driver::wire` in `chat.rs` get one arm each.
- **Launcher** (`chat/launch.rs`, nothing to add): in a WSL chat it runs the agent's Linux program, else its Windows one through WSL; in a Windows folder the Windows program, else the Linux one of the default distribution. Always the real program (a native `.exe`, never npm's `.cmd` or shell shims, whose interpreter can be the wrong side's). The launch script reports which one it started and the chat's folder as that program names it (`Inner::agent_folder`, a `\\wsl.localhost` path for a Windows program in WSL): send that folder wherever the protocol takes a `cwd`.
- **The agent's shell is the soup** (nothing to add for a Linux program): every Linux program gets `BASH_ENV` = `~/.compi/shell/compi-agent.sh`, so its `bash -c` commands land where the chat is, as typed in a Compi terminal there (PowerShell commands in PowerShell, `cd` taking Windows paths). A Windows program in a WSL chat runs its commands through Compi's agent shell (`crate::agent_shell`, the daemon linked as `bash.exe` in the data folder), which runs them in the distribution through the same hook and puts the Git Bash paths of Windows drives (`/c/…`) where WSL mounts them. Set `windows_bash` to the variable the agent reads for its Bash (Claude Code: `CLAUDE_CODE_GIT_BASH_PATH`; OMP's extension reads `COMPI_AGENT_SHELL`). An agent with no such variable (Codex's Windows build always uses PowerShell) keeps its own shell; it is told only how Linux paths map for its file tools (`windows_paths_note`). Never add prompt text telling an agent to wrap its commands.

## 1. Capture the protocol from the agent's own binary

Work from what the installed program says, not from docs.

- If it can describe itself, generate the schema from the exact version you target and keep it: `codex app-server generate-json-schema --out target/agent-notes/codex-schema` gave every request, response and notification of Codex 0.169.
- Record a real session: start it the way Compi will (its JSON mode on standard input and output), send one prompt that runs a command, edits a file and asks permission, and save every line (`target/agent-notes/claude-probe.jsonl` is Claude Code's). Note its version.
- Find how it runs commands on each side: watch the processes while it runs a long command (on Windows, `Get-CimInstance Win32_Process | Select ProcessId,ParentProcessId,CommandLine`). That is how Claude Code's `bash.exe -c "source <snapshot> && eval '<command>' && pwd -P >| <cwd file>"` was found.
- Find how it is installed on each side and where the real program ends up (npm's `node_modules/.../<program>.exe`, a standalone `~/.<agent>/…/bin`, `~/.local/bin`): that is the entry's `linux_folders` and `windows`.

## 2. Write the driver against the recordings

- Follow `claude.rs` (stream of messages, Compi starting each turn) or `acp.rs` (JSON-RPC with sessions) as the closest shape. `codex.rs` is JSON-RPC with server requests for approvals.
- Map onto the core's contract, which is all the client renders (`ChatState`): reply and reasoning text (`Inner::append`), tool calls (`ChatTool` with kind `execute`/`edit`/`read`/`search`/`fetch`/`think`, input, output, `ChatDiff`s for edits), permissions (`ChatPermission` with `allow_once`/`allow_always`/`reject_once` options), questions, options by category (`model`, `thought_level`, `mode`), usage, title, the session ID, and the turn's end through `prompt_ended`.
- Map Compi's permission levels onto the agent's own in the entry (`approval_modes`), and pass the chosen one at launch or session start. `Harness` keeps the agent's own setting; if the agent does not read its own setting in this mode, look it up in `configured_approval` (OMP does).
- Controls the agent can't support stay off: no `steer` means Compi queues prompts; no `switch_session` means Resume… restarts it on the session.
- Tests feed the recorded lines through `handle` on an `Inner` and check the entries, permissions and the JSON written back. No network, no processes.

## 3. Add the entry

Add the `ChatHarness` variant, its `AgentEntry`, `Driver` variant and the two `Driver` arms. A mark is either the agent's own SVG in `crates/compi-client/assets/brands/` (`AgentMark::Brand`) or a model mark in `assets/models/` drawn in a colour (`AgentMark::Model`). Then build: every agent list picks it up.

## 4. Run the smoke test on both sides

Against a running dev instance (`cargo dev`), from WSL:

```sh
/mnt/c/Users/johns/.cargo/bin/cargo.exe run -p compi-client --example agent-smoke -- --agent <program> --env wsl
/mnt/c/Users/johns/.cargo/bin/cargo.exe run -p compi-client --example agent-smoke -- --agent <program> --env host
```

It opens a chat in a throwaway folder (`/tmp/…` in the default distribution, `%TEMP%\…` on Windows), asks the agent to run a command that writes the operating system's name and to create a file with its edit tool, allows every permission, and checks that the command ran on the chat's side (`Linux` in WSL, Windows on the host), that the edit landed, and that the transcript shows the command. It prints PASS or FAIL per check, then removes the tab, the thread and the folder (`--keep` keeps the folder). It spends a little of the agent's usage. Run it for every agent you touched, on both sides; a combination that can't pass (an agent installed on one side only) is reported, not skipped silently.

## 5. Docs checklist

- `docs/user/USAGE.md`: Chat with an agent, Default agent (agent list, New chat with …), `compi chat --agent`, program status, the WSL note.
- `docs/user/CONFIGURATION.md`: `[chat] agent` values, `[chat.<program>]` with its model names, project `agent` values, what Approvals map to for the agent.
- `docs/dev/COMPLETED.md`: what was added and the smoke results.
- This recipe, if the agent needed anything new.
