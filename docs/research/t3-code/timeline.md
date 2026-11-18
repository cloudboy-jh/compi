# How T3 Code's thread timeline behaves

## 1. Rows and grouping (`MessagesTimeline.logic.ts:1595-2420`)
- **Consecutive tool calls form one group** when they share a run and an attempt. These always break a group and render on their own: errors, system notices, notifications, todo lists, answered questions, compaction, and pending approvals (`:2101-2131`).
- **One finished tool call** gets a single row with a specific label, such as `Ran {program}` or `Read file +N more`. **Two or more** get a collapsed `work-toggle` header that summarises up to two action types plus a count ("Ran 2 commands and sent messages to 3 threads"). Commands, edits and orchestration outrank reads and status checks. The header carries a failure mark if the latest tool call failed (`:2148-2195`; `docs/user/activity-log.md:3-17`).
- **Hidden by default inside a group:** finished tool calls with a neutral status. Reasoning shows only if it has text (`workEntryIsVisibleInGroup`, `:301`).
- **Images the agent viewed** stay visible as a thumbnail strip under the group even while it is collapsed (`viewed-images` row, `:719`).
- **Sub-agents** started in the same provider turn merge into one card: "N subagents", up to three avatars, a status line, elapsed time, and a chevron. Expanding it lists each child, and each child can be opened (`TimelineV2EventRow.tsx:55-175`). Sub-agent cards stay unfolded while any child is still running (`logic.ts:900`).

## 2. Folding a finished turn (`deriveTurnFolds`, `:1160-1290`)
- **When a turn settles**, everything before the agent's last message collapses into one row: **"Worked for {duration}"**. If you stopped it, the row reads "You stopped after {d}" instead.
- **Work after a steer** folds under its own row, "Continued for {d}". Each segment opens separately.
- **Never folded:**
  - the turn that is still running or streaming
  - failed or interrupted runs
  - pending approvals
  - answered questions and notifications
  - persistent resource cards and live sub-agents
  - the latest compaction divider
  - replies that were cut short by a steer
- **Attempts replaced by a later attempt** fold into "Superseded attempt" (`:2002`).
- **Zen mode** hides the agent's in-between messages and disables folding entirely (`:1678`).

## 3. Live activity while the agent works
- **Working row:** sits at the top of the active turn and shows **"Working for {ticking timer}"**. The timer updates the text directly instead of re-rendering the row (`TimelineWorkGroupRows.tsx:127, 385-410`). It switches to "Compacting for…" or "Setting up worktree…" with a shimmer. If worktree setup is still running in the background, a "Background setup" chip opens a progress popover.
- **Live row (`work-live`):** the tail of the active turn shows just one row for the latest running tool call, with a shine effect ("Running cargo"). The latest thought appears above it, clamped to 4 lines. Clicking the row expands the full group (`:571-680`; `logic.ts:1740-1790`).
- **"Thinking" row:** shown when nothing visible is running. It can be replaced by:
  - a preview of the live answer
  - the provider's "starting" label
  - "Waiting on N subagents" (`:2373-2410`)
- **When the latest tool call fails mid-turn**, the "Thinking" row becomes the clickable disclosure for that group (`:2378`).
- **Pending approvals** suppress the "Thinking" row, so nothing claims to be thinking while it waits on you.

## 4. Expanding groups and individual tool calls
- **Expanded group:** an inner scroll list with a maximum height of about 18rem, plus the height of any entries opened inside it. Edges fade when there is more to scroll. Scroll position and which entries are open are remembered per group and restored when you come back to the thread (`TimelineWorkGroupRows.tsx:182-345`).
  - It follows new entries only if you were already at its bottom and the earlier entries are unchanged (`shouldFollowWorkGroupAppend`, `logic.ts:336`).
- **Single entry:** a row you can toggle with Enter or Space. Its chevron rotates. Buttons inside the row stop the toggle from firing (`TimelineWorkEntryRow.tsx:281-697`). What the expanded body shows depends on the type:
  - **reads:** the absolute paths
  - **reasoning:** full markdown, streamed while live
  - **questions:** the Q&A history with attachments
  - **everything else:** an item inspector
- **Command output** (`CommandOutputPanel.tsx`):
  - fetched only while the row is mounted and expanded
  - streams while the command runs, with ANSI colours
  - pinned to the bottom unless you scroll more than 8px up
  - has a fixed maximum height
  - shows "Earlier output not shown" when truncated
  - states: loading ("Loading output…"), error ("Couldn't load output"), finished with nothing ("No output."), running with nothing yet (blank)
- **Inline links:** "Open chat" on thread creation, "Open subagent" on child notifications.
- **Opening a disclosure near the bottom** scrolls only enough to reveal it, never past the top inset (`timelineScrollAnchoring.ts:94`).

## 5. Approvals and questions
- **Inline approval row** (`TimelinePendingApprovalRow.tsx`):
  - per-kind icon (command / file change / file read / permission / app access)
  - full detail as selectable, focusable code
  - reason and app name
  - "Waiting for approval · Decline · **Approve** · More ▾"; the More menu holds "Always allow this session" and "Cancel"
  - options can come from the provider, and options with warnings show a ⚠ tooltip
  - all buttons are disabled while a response is in flight
  - if the provider process is gone: "Interrupt or restart the run to respond"
  - a hover popover explains why a request asked in Full access
- **Approvals bypass folds:** they are pulled out of folded turns (`logic.ts:1712`).
- **Sub-agent approvals** show above the composer as "Approval needed from {title}". They have their own in-flight state and an inline `role=alert` error, and the stack scrolls at about 20rem (`ChildThreadApprovals.tsx`; `permission-modes.md:28-30`).
- **Text reply as decline (Claude):** typing a text reply while an approval is pending declines it with your feedback (`permission-modes.md:52-54`).
- **Questions** are a collapsible banner above the composer (`ComposerPendingUserInputPanel.tsx`):
  - header with an "i/N" count; one question shows at a time
  - keys **1–9** pick an option; ignored in text fields, menus and dialogs, or when collapsed
  - a single-select pick shows as selected immediately, then auto-advances after 200ms
  - multi-select uses checkboxes
  - an optional "Dismiss without answering"
  - collapsing it shows "header · question" on one line
- **Answers stay answerable** after the provider turn ends (message mode).
- **Attachments** are kept per question, and the draft survives a failed submit (`question-attachments.md`).

## 6. Scroll-follow (`timelineViewport.ts`, `useTimelineFollow.ts`)
- **Following states:** following → released(at end) → released(away from end).
- **What releases following:**
  - wheel up, or Up/PageUp/Home, but only if the content can actually scroll up
  - a scrollbar drag
  - downward input, but only outside a 40px band at the end, because streaming makes "at end" flicker
- **Resuming:** following resumes only after you leave the end and come back. A scroll key pressed while already at the end doesn't re-arm it (`:258-276`).
- **Programmatic scrolls** (anchoring, restoring) are flagged so they don't count as the user scrolling.
- **New run:** while following, its user message is pinned near the top instead of the view jumping to the bottom (`useTimelineFollow.ts:246-267`).
- **"Scroll to end" pill:**
  - appears after a 150ms delay and hides immediately
  - can show a summary of what is waiting at the end
  - takes no focus on pointer-down (`TimelineOverlays.tsx:26`)
- **Per-thread memory:** each thread remembers its scroll position, whether it was at the end, and which folds and groups were open (up to 100 threads, LRU), so switching back restores all of it (`timelineScrollAnchoring.ts:115-150`).

## 7. Changed files at the end of each turn (`ChangedFilesTree.tsx`, `TimelineAssistantRows.tsx:430-521`)
- **Card under the turn's final message:** "N changed files +a −d", with a sticky header, an expand/collapse-all-folders toggle (saved per thread and run), and "Open diff".
- **Tree:** folders with their own diff stats. Clicking a file opens the diff at that file. Right-click opens a file context menu.
- **Large lists:** the tree is virtualized after 250 rows, and "N more files in the full diff" covers files the card doesn't list.
- **Changes from a single tool call** also have **Open diff** (`activity-log.md:6`).

## 8. Message actions, copy, quote, rewind
- **Assistant final message:** status badge if not completed, Copy (only once streaming is done, and only if there's text), Fork from this response (pulses while busy), Reply to this response, and a timestamp. These appear on hover or focus, and always on touch screens (`TimelineAssistantRows.tsx:236-275`).
- **Copy** shows a check mark and disables itself briefly; errors appear in a toast anchored to the button (`MessageCopyButton.tsx`).
- **Cite:** selecting assistant text shows a floating **Cite** button. Tab moves focus to it and Esc dismisses it. Over the length limit it is disabled and reads "Shorten selection" (`AssistantSelectionToolbar.tsx:124-219`).
- **User message:** "Edit from here", Copy, and Copy prompt link (`TimelineUserMessageRow.tsx:374-381, 1177`).
  - Edit is disabled while the agent is working or a revert is running.
  - It opens "Edit from here?" with three choices: Cancel / **Revert files too** (only in a worktree with a ready checkpoint) / Revert and keep changes.
  - Reverting puts your prompt and attachments back in the composer, with focus at the end.
  - A running turn or an unsupported provider gives an inline error instead (`useTimelineRewind.ts:95-175`; `ChatOverlayHosts.tsx:207-262`).

## 9. Banners and errors
- **Banner lane above the timeline:** provider status and thread errors. Dismissing one hides it for the rest of the session, keyed by the error. The ChatGPT usage-limit banner includes "Copy request for admin" (`ChatBannerLane.tsx`; `ThreadErrorBanner.tsx:66-134`).
- **Errors inside the timeline:** failed items show as a red row with the full message and a timestamp that is always visible. Usage limits show as a warning with the reset time in your local time zone. A failed workspace preparation offers **Retry** (`TimelineWorkEntryRow.tsx:322-381`).
- **End-of-timeline status rows:**
  - **Background work:** "Running · {cmd}" with a ticking timer, **Details** (a dialog with live command output and "Open subagent"), and Stop.
  - **Stopping:** if the agent doesn't respond to Stop, the row offers "Force stop".
  - **Sub-agent waiting:** "Open approval/question".
  - **Goal:** Resume or Clear.

  (`ThreadActivityBanners.tsx:50-300`, `BackgroundWorkDetails.tsx`)
- **Tool call visibility setting:** **Hide successful** shows a call while it runs and keeps it only if it fails. **Hide all** shows "Thinking" instead. Approvals, questions and errors always show (`activity-log.md:19-30`).

## Design rules worth copying
- **One live row per turn.** The full history is one click away.
- **Fold by default only once a turn has settled.** Never fold anything that is running, failed, or waiting on the user.
- **Output follows only when you're at the bottom,** in the timeline, inside expanded groups, and in command output alike. One accidental scroll mustn't break following, and a key press at the end mustn't re-arm it.
- **Expand/collapse state and scroll position survive switching threads.**
- **Elapsed timers update the text directly,** without re-rendering the row.
