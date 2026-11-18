# T3 Code: how threads store data, map to provider sessions, persist history, and hand off context

## Facts

**What a thread stores**
- **AppThread** is the conversation the user sees. Its fields include `projectId`, `modelSelection` (with `instanceId`), `runtimeMode`, `interactionMode`, `branch`, `worktreePath`, `activeProviderThreadId`, `lineage`, `archivedAt` and `deletedAt` (`docs/orchestration-v2/core-graph-and-data-model.md:120-139`). Code also uses `settledOverride`, `settledAt`, `snoozedUntil`, `pinnedAt`, `pullRequests` and `limitRecovery` (`apps/server/src/orchestration-v2/Orchestrator.ts:4669-4705`).
- The thread owns these records:
  - **Runs**: one per user turn. Each has `ordinal`, `provider`, `providerThreadId`, `contextHandoffId` and `checkpointId`.
  - **RunAttempts**: retries, steering restarts and recovery.
  - **ExecutionNodes**: tools, approvals, subagents.
  - **ProviderThreads**, **ProviderSessions**, **ContextTransfers**, **ContextHandoffs** and **CheckpointScopes** (`core-graph…md:5-15, 160-176`).
- In the glossary, "Activity" means a timeline item that is not a message (`docs/internals/glossary.md:16`).
- Checkpoints are hidden Git refs (`docs/internals/overview.md:86`).
- Terminals are keyed by `threadId` in the terminal manager (`apps/server/src/terminal/Manager.ts:238`), not stored on the thread.

**How a thread maps to provider sessions over time**
- A **ProviderThread** is the provider's own conversation handle (`nativeThreadRef`). A **ProviderSession** is the live process. Each is tagged with a `providerInstanceId`.
- One app thread can hold several provider threads. Example: runs 1–5 on Codex C1, runs 6–8 on Claude L1, then run 9 back on C1 (`docs/orchestration-v2/provider-switching-and-context.md:28-41`).
- Sessions can be shared or pooled across threads (`ProviderSwitchService.ts:163-170`; `ProviderRuntimeRecoveryService.ts:206`).
- A session and provider thread are created lazily, on the first run, not on `thread.create` (`feature-lifecycles.md:5-25`).
- When the selection changes, `ProviderSwitchService.plan` decides what happens:
  - Same instance, different model: the adapter's `planSelectionTransition` either applies it in place or uses `apply_on_next_turn`.
  - `restart_and_resume` releases only the replaced session.
  - `create_with_handoff` (instance change) releases sessions on other instances (`ProviderSwitchService.ts:67-178`).
  - An instance change while a run is active is rejected with `OrchestratorProviderSelectionBusyError` (`Orchestrator.ts:4468-4479`).
- Other events that detach sessions: a worktree change, and a runtime-mode change when the session can't switch in place (`Orchestrator.ts:4482-4496`).
- A new provider thread is created when:
  - resume fails, or settings are incompatible (`feature-lifecycles.md:126-131`);
  - the target has no room for the handoff, so it starts fresh with a bounded selection (`docs/user/portable-handoffs.md:21-27`);
  - the thread was migrated from V1 (`docs/user/thread-migration.md:27`).

**Who persists history**
- **T3 is the source of truth.** Events and projections are committed in one SQLite transaction (`statev2.sqlite`) by `EventSink` (`overview.md:45-55`). "The app thread remains the source of truth for full history" (`provider-switching…md:80`).
- The event log is pruned after about a day, so the projections are the durable record (`overview.md:62-72`).
- Each provider keeps its own native transcript. T3 keeps only the ref and resume cursor needed to reopen it (`feature-lifecycles.md:277-292`).

**Forks, side chats and sub-agents**
- **Forks** have `lineage.relationshipToParent: "fork"` and a `ContextTransfer` with `status=pending`. Creating a fork starts no session, provider thread or context.
- On the first dispatch, a fork to the same provider uses the provider's native fork if available (Codex `thread/fork`). Otherwise T3 builds a portable handoff. Active source runs are rejected (`thread-lineage-and-context-transfer.md:139-152, 279-286`).
- **Merge-back** builds a delta from the base point to the fork's source point (`:180-187`).
- **Side chats** are forks with `lineage.sideChat = true`, shown only inside the parent (`:271`). **Bring back** works only while the parent is idle. Deleting the parent deletes its side chats (`docs/user/thread-sidebar.md:300-320`).
- **Subagents** come in two kinds:
  - Native: the provider spawns them and they become `subagent` nodes.
  - App-owned: the agent delegates through a T3 tool, which creates a child AppThread (`relationshipToParent: "subagent"`, `childThreadId`). Its result comes back to the parent as a context transfer (`thread-lineage…md:62-76`).
- Users can't message subagent threads; their approvals surface in the parent (`thread-sidebar.md:282-284`).
- Stopping a thread stops its children depth-first through `thread.stop` (`feature-lifecycles.md:253-260`).

**Server restart mid-turn**
- On startup, `ProviderRuntimeRecoveryService.reconcile` ends any run that is still `preparing`, `starting`, `running` or `waiting`. It also stops sessions, closes requests, and retires or requeues outbox effects (`ProviderRuntimeRecoveryService.ts:64-98`).
- `RestartContinuation` then sends a synthetic "Continue where you left off…" prompt (`RestartContinuation.ts:37`), but only if all of these hold (`:44-90, 165-232`):
  - the provider thread has a strong native ref on the same instance;
  - the thread is not archived, deleted, settled or snoozed;
  - nothing is pending: no approval, answer or secret.
- It gives up after 3 chained recoveries (`:40`). The setting `continueThreadsAfterServerUpdate` is on by default (`docs/user/updating.md:67-83`).
- Terminal commands may still be interrupted.

**Archive, settle and delete**
- **Archive** sets `archivedAt` and clears guest share and PR watches. It is rejected while app-owned delegated tasks are running (`Orchestrator.ts:4239-4256, 4669-4676`). It detaches **all** provider sessions, skipping the background-work check that other session-changing commands run (`:4480-4509`).
- [INFERENCE] I found no code that closes terminals on archive.
- **Settle** does the following:
  - clears the pin, snooze and auto-resume of usage-limit recovery;
  - detaches sessions;
  - dismisses unanswered async questions;
  - closes terminals idling at a prompt, keeping their output (`Manager.ts:3911`);
  - stops command terminals only for actions with `stopOnSettle`;
  - runs `runOnSettle` scripts in the thread's own worktree (`thread-sidebar.md:165-183`).
- Auto-settle rejects threads with live or blocked work (`overview.md:96-101`).
- **Delete** sets `deletedAt` (a soft delete) and cancels active runs, attempts, nodes and pending requests (marked `not_resumable`). It disposes delegated cohorts and detaches sessions, revoking their MCP credentials. Its effects are `scheduled-tasks.cleanup`, `terminal.cleanup`, `preview.cleanup` and `attachment.cleanup` (`ThreadDeletion.ts:57-245`). Scratch folders are kept (`thread-sidebar.md:37`).

**How handoff selection is computed**
- **Which runs are covered:** completed runs with ordinal greater than the target provider thread's last delivered run, up to the latest run. The list is empty if the instance is unchanged or the provider can resume across instances (`Orchestrator.ts:2741-2753`).
- **Strategy:** `full_thread_summary` if the target has no `nativeThreadRef`, otherwise `delta_since_target_last_seen` (`:2754-2765`, `8719-8781`).
- **Budget** (`packages/provider-core/src/server/handoffBudget.ts:115-143`):
  - formula: `min(cap 16k [T3CODE_CONTEXT_HANDOFF_TOKEN_CAP, 1,024–64k], 64 KB, window − native usage − request − attachments − reserve)`;
  - window is the smaller of the known model window and reported usage, defaulting to 128k;
  - reserve is `max(16k, window/4)`, capped at `window/2`;
  - images cost 8,192 each, other files 4,096 each;
  - 1 byte counts as 1 token.
- **What is included:** user and assistant messages, commands, errors, interrupts, file changes and plans (`:145-187`).
- **Selection order** (`selectHistory`, `:243-298`): latest user message, latest assistant message, first user message, then newest to oldest. Selected items stay intact; omitted item IDs are listed.
- **Recovering omitted items:** the coverage preamble tells the agent to call `t3_thread_read({threadId, view:"activity", itemId, maxCharsPerItem:4000})` with `textOffset` paging (`:300-311`). The tool is defined at `apps/server/src/mcp/toolkits/orchestrator/tools.ts:252`.
- **Docs disagree:** `docs/internals/context-handoffs.md:6-11` describes a 240-character `compactText` summary in `ContextHandoffServiceV2`. The code and the user docs use intact messages within a budget. The 240-character rendering seems to apply to summary lines such as merge-back deltas (`ContextHandoffService.ts:222, 471`) [INFERENCE].

## Implications for Compi

- **Split the thread from provider conversations.** Keep a daemon-owned chat thread (transcript plus activities) separate from a list of agent conversation handles: `{agent, instance, nativeSessionId, lastDeliveredTurn}`. Switching agents or models mid-thread then means adding or reusing a handle, not forking the thread. Return to an old handle with a delta since its last turn.
- **The daemon must persist the transcript itself.** ACP agents keep their own history, but restarts, switches and imports have to rebuild context from Compi's store. Ship a thread-read tool (over MCP or ACP) so agents can fetch omitted items rather than relying on summaries.
- **Copy T3's budget formula and selection order.** They are concrete, cheap, and don't depend on any agent. Never trim the user's request; fall back to a fresh agent session when the target has no room.
- **Make forks and side chats lazy.** Record lineage plus a source point, and create a session only on the first message. Side chats are forks with a hidden flag, plus a bring-back step that delivers a delta.
- **Define lifecycle effects on terminals and sessions explicitly.** Delete cancels work and cleans up terminals. Settle closes idle terminals and stops only opted-in commands. Archive detaches agent sessions and refuses while children are running.
- **Restart recovery:** mark runs that were in flight as terminal on startup, and auto-continue only when there is a strong resumable session ref and nothing is waiting on the user. Cap the retries at 3.
