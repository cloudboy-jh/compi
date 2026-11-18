# T3 Code: what each agent driver shows, and what generic ACP loses

## Summary

T3 normalizes every driver into one set of turn items (`packages/contracts/src/orchestrationV2.ts:1714-1945`): `command_execution` (input, output, exitCode), `file_change` (diffStr, oldStr, newStr), `file_search`, `web_search`, `dynamic_tool`, `reasoning`, `todo_list`, `proposed_plan`, `user_input_request`, `approval_request`, `subagent`, `compaction` and `checkpoint`. Each driver also declares a capabilities object, and the UI decides what to offer from those flags rather than from the driver name. The capabilities objects are at:
- Claude: `apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts:514-608`
- Codex: `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts:261-355`
- OpenCode 2.x: `packages/provider-opencode/src/server/v2/adapter.ts:129-218`
- Pi: `packages/provider-pi/src/server/adapter.ts:109-198`
- ACP: `packages/provider-acp/src/server/adapter.ts:727-819`

T3 builds the file diffs it shows from its own checkpoints for every provider. It even tells Codex not to send `turn/diff/updated` (`docs/internals/providers.md`, "Provider diagnostics").

## Per driver

**Claude: the `@anthropic-ai/claude-agent-sdk` TypeScript SDK** (`ClaudeAdapterV2.ts:67`)
- **Tool kinds** come from a fixed name table (`:3580-3600`):
  - Bash → `command_execution`
  - Edit, MultiEdit, NotebookEdit and Write → `file_change`
  - WebSearch and WebFetch → `web_search`
  - Read, Grep, Glob, Agent, TodoWrite and anything unknown → `dynamic_tool`
- **Command output** is not streamed (`streamsToolOutput: false`); it arrives when the tool finishes.
- **Approvals:** the SDK's `canUseTool` callback (`:12220-12400`). Permission mode can be changed with `setPermissionMode` (`:1790`), but the capability flag still says no mid-session mode switch.
- **Questions:** `AskUserQuestion` is captured and answered by T3 (`:12263`).
- **Plans:** `ExitPlanMode` becomes a `proposed_plan` card, and T3 then denies the tool on purpose (`:12333`).
- **Todos:** `TodoWrite`, plus `TaskCreate`/`TaskUpdate`/`TaskList` when `CLAUDE_CODE_ENABLE_TODO_TOOLS=1` (`:5491`; `docs/user/providers-claude.md`, "Task lists").
- **Sub-agents:** the `Agent` tool, settled by `task_notification` (`:4012`). They have no child thread IDs.
- **Steering:** active steering, which interrupts running tools (`activeSteeringInterruptsTools`).
- **Rewind and fork:** rewind uses `resumeSessionAt`; fork uses `forkSession`.
- **Models and effort:** model switch with `setModel` (`:1760`); effort is supported. Changing thinking needs a new process (`:2064`).
- **Context usage:** `getContextUsage` (`:273`).
- **Slash commands:** from `initializationResult().commands` (`ClaudeProvider.ts:536,600`).
- **Skills:** read from the skills folders and picked with `$` (`providers-claude.md`).

**Codex: `codex app-server`, JSON-RPC over stdio** (`packages/effect-codex-app-server`, with a schema generated in `_generated/schema.gen.ts`)
- **Tool items:** commandExecution, fileChange, mcpToolCall, dynamicToolCall, collabAgentToolCall, webSearch, imageView, imageGeneration and contextCompaction (`CodexAdapterV2.ts:5352-5750`).
- **Streaming:**
  - command output via `item/commandExecution/outputDelta` (`:4848`)
  - reasoning summary and full reasoning text (`:4868`, `:4871`)
  - plan text via `item/plan/delta` and `turn/plan/updated` (`:4875`, `:4890`)
- **Usage:** `thread/tokenUsage/updated` and `account/rateLimits/updated`.
- **Approvals:** for commands, file changes and permissions, plus legacy exec/applyPatch approvals, MCP elicitation and `item/tool/requestUserInput` (`:5858-6283`).
- **Async questions** arrive as notifications and are answered with a new user message (`providers.md`, "Protocol traps").
- **Steering:** `turn/steer` (`:8159`).
- **Resume, fork, rollback:** `thread/resume` and `thread/fork` (fork from a given turn uses `lastTurnId`). Rollback uses `thread/revert` (`CodexThreadRevert.ts:40`).
- **Sub-agents** get real thread IDs, and T3 can wait on them, close them, and fork from them.
- **Models and skills:** `model/list` and `skills/list` (`CodexProvider.ts:526,773`).
- **Slash commands:** `/compact`, `/goal` and `/feedback` (`CodexProvider.ts:1162`).

**OpenCode: an HTTP server**
- **Transport:** 1.x runs `opencode serve --hostname --port` through `@opencode-ai/sdk` (`OpenCodeRuntime.ts:608`). 2.x runs `serve --service` and reads events from the `/api/event` SSE stream (`OpenCode2Client.ts:178`).
- **Tool kinds** come from substring matches on the tool name (`toolItems.ts:38-64`):
  - names containing bash/shell → command
  - edit/write/patch → file change, with oldString/newString and a diff from the tool's metadata
  - glob/grep/search/lsp → `file_search`
  - web/codesearch → `web_search`
- **Background shells:** output is pulled through a shell-jobs endpoint (`v2/adapter.ts:2836-2870`).
- **Todos:** `todowrite` becomes a `todo_list` (`:1368`).
- **Questions:** the `question` tool becomes structured questions (`:488-512`).
- **Approvals:** `permission.asked` events (`:3363`).
- **Sub-agents:** the `subagent` tool, linked to child sessions (`:2789-2830`).
- **Steering, rollback, fork:** all native (`:4963`, `:5294`, `:5344`).
- **Commands:** run with `session.command`.
- **Not available:** plan cards (`emitsProposedPlan: false`) and streamed tool output.

**Pi: `pi --mode rpc`, JSON lines over stdio** (`mcpInjection.ts:23,275`)
- **Tool kinds:** bash/powershell → command, with streamed output from `tool_execution_update` (`adapter.ts:1365`). edit/write → `file_change` using a `details.patch` diff. Everything else → `dynamic_tool`.
- **Reasoning:** `thinking_delta` events.
- **Approvals:** T3 injects its own Pi extension that hooks `pi.on("tool_call")` (`mcpExtensionSource.ts:363`). Its prompts arrive as `extension_ui_request` confirm, which T3 shows as an approval; other dialogs become questions (`adapter.ts:1869`).
- **Permission modes:** no Auto mode (`status.ts:57`), and changing the mode restarts the session.
- **Not available:** todos and plans (all flags false).
- **Steering:** `prompt` with `streamingBehavior:"steer"` (`:3613`).
- **Fork, resume, rollback:** fork runs the CLI with `--fork`; resume uses `switch_session`; rollback is native.
- **Models, effort, usage, commands:** `get_available_models`, `set_thinking_level`, `get_session_stats`, `get_commands`.
- **Sub-agents:** only through Pi's example extension, and those children cannot be reopened as threads (`providers-pi.md`).

**Generic ACP: JSON-RPC over stdio, v1 and v2 preview**
- **Tool kinds:** `execute` → command; edit/delete/move → file change, using ACP diff content (`runtimeModel.ts:628`); `search`/`fetch` → `web_search`; `read` and anything else → `dynamic_tool` (`runtimeModel.ts:796-812`).
- **Also covered:** plan entries become a todo list; `agent_thought_chunk` becomes reasoning; `usage_update`; slash commands from `available_commands_update`; models and effort from `configOptions` or `session/set_model`; modes via `session/set_mode` (`AcpSessionRuntime.ts:2763-2839`); form/URL elicitation (`adapter.ts:2598`); session load and resume.

## What generic ACP loses, with evidence

- **No sub-agents.** `supportsSubagents: false`; the user doc says ACP has "no portable native subagent-lineage contract" (`providers-acp.md`). Codex, Claude, OpenCode and Pi all report sub-agents.
- **No real conversation rewind.** `providerCanRollbackConversation: false` (`adapter.ts:806`). After a rollback T3 starts a fresh agent session and replays the kept history. Codex, Claude, OpenCode and Pi rewind natively.
- **No steering mid-turn by default.** `supportsActiveSteering: false`; the generic path interrupts and restarts the turn. Only per-agent flavors add real steering, and a steer is dropped if the agent doesn't read it within 2 seconds (`:9327-9380`).
- **Fork depends on the agent.** It is off unless the agent advertises `session/fork`, and fork-from-a-turn needs a flavor hook (`:844-889`).
- **No plan cards, and no plan or command-output streaming.** `emitsProposedPlan: false` and `streamsPlanText: false`. Claude's ExitPlanMode and Codex's plan items are lost. Command output is only what the agent reports; T3 doesn't run or stop the agent's terminals (Devin is the exception).
- **Weaker approvals.** Request and item IDs are only "weak". Enforcement is `client-boundary`, not `native`: T3 auto-answers requests according to the thread's permission mode. There is no apply-patch approval and no approvals from sub-agents.
- **Coarse tool kinds.** File searches (`search`) are shown as *web* search, and reads become generic tools. Claude and OpenCode give typed `file_search`/read labels.
- **No system or developer context.** `acceptsSystemContext: false` and `acceptsDeveloperContext: false`, so a provider switch can only hand over context as a synthetic user message. There are also no dynamic tool callbacks.
- **Model switching mid-session** works only if the agent advertises a model option (`negotiatedCapabilities`, `:861-870`).
- **Missing extras:** no usage-limit or rate-limit windows, no `/goal`, and no image generation (Codex-only). Also no skill discovery: only commands the agent names with `$` show up as skills.

## Capability table

| Capability | Claude | Codex | OpenCode | Pi | ACP |
|---|---|---|---|---|---|
| Protocol | TS Agent SDK | app-server stdio JSON-RPC | HTTP + SSE | RPC JSONL stdio | ACP stdio JSON-RPC |
| Typed cmd/edit/search | cmd, edit, web | all + MCP + images | cmd, edit, file/web search | cmd, edit | coarse |
| Streamed cmd output | ✗ | ✓ | shell poll | ✓ | as agent reports |
| Reasoning | ✓ | ✓ (summary + raw) | ✓ | ✓ | ✓ |
| Approvals | native | native, richest | native | via injected extension | client-side, weak IDs |
| Questions | AskUserQuestion | async + requestUserInput + elicitation | question tool | extension dialogs | elicitation |
| Todos / plan card | ✓ / ✓ | ✓ / ✓ | ✓ / ✗ | ✗ / ✗ | ✓ / ✗ |
| Sub-agents | ✓ (no IDs) | ✓ (threads) | ✓ (child sessions) | extension only | ✗ |
| Active steering | ✓ | ✓ | ✓ | ✓ | flavor-only |
| Rewind / fork | ✓ / ✓ | ✓ / ✓ | ✓ / ✓ | ✓ / ✓ | replay / if advertised |
| Model switch mid-session | ✓ | ✓ | ✓ | ✓ | if advertised |
| Usage / limits | ✓ / ✓ | ✓ / ✓ | ✓ | ✓ | usage_update |
| Commands / skills | ✓ / ✓ | ✓ / ✓ | ✓ / ✓ | ✓ / ✓ | ✓ / `$`-named only |

## Can each be driven from Rust?

- **Codex, OpenCode, Pi and ACP: yes.** Each is a process speaking stdio JSON or HTTP/SSE. Pi's approvals do need T3's TypeScript extension file installed in Pi, but it is just a file and Pi runs it.
- **Claude: not directly.** T3 depends on the Node SDK and even runs a worker (`runClaudeWorker`, `:1375`). From Rust, Compi would need one of:
  - a Node sidecar that runs the SDK, or
  - reimplementing the CLI's stream-json control protocol [INFERENCE: this is the protocol the SDK uses under the hood], or
  - `claude-agent-acp`, which `providers-acp.md` mentions and which falls into the ACP losses above.

## Implications for Compi

- Use native drivers for Codex (app-server), OpenCode (HTTP) and Pi (RPC). Each gives sub-agents, real rewind and steering that plain ACP does not.
- For Claude, decide early between a Node SDK sidecar and `claude-agent-acp`. Going through ACP loses plan cards, sub-agents and native rewind.
- Copy T3's per-driver capability flags so the UI hides fork, rewind, steering, plan and sub-agent controls for drivers that can't do them, instead of failing at runtime.
- Build diffs from Compi's own file checkpoints, as T3 does, rather than from each provider's diff events.
- Pass providers' own approval option IDs through unchanged. Only offer permission modes the agent enforces itself; don't fake them by answering approvals in Compi.
- OMP over ACP will carry the ACP limits above unless Compi adds an OMP-specific layer on top of the shared ACP driver, the way T3 does for Grok and Antigravity.
