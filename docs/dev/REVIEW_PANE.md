# Review view (design draft)

Status: in discussion (started 2026-11-23). Nothing here is built. Product order lives in the vault's `feat and bugs.md`.

## Problem

Agents commit to `main` often. Their work ends up spread across many commits, so reviewing it means either checking every diff before it is committed or reading commit after commit afterwards. Nothing shows, at a glance, what happened in each project since you last looked and which agent did it.

## What Review is

Its own view: a quick list, not a dashboard. It answers "what happened where, and who did it" and hands off to the existing diff view for detail. **Changes** stays exactly as it is.

One block per project; a project with nothing new folds to one line.

- **Project line:** project icon and name, branch, commits not yet pushed (↑), commits since review, +/−, the marks of the agents that worked there, **Mark reviewed**.
- **Rows**, newest first:
  - **Uncommitted:** file count; opens in Changes.
  - **Commits:** icon, agent mark, message, short id, age, +/−. Click opens the commit's diff, as `compi diff COMMIT` does.
  - **Pushed:** a divider row ("pushed to origin"); rows above it are local only.
- **Project line click:** every commit since review plus uncommitted work as one diff.

### Row icons

A plain commit's icon says what it changed: the language icon (the ones Files and Changes already use) of the file type with the most changed lines. Hover lists the other types. No generic commit dot.

Git events keep their own icons: merge, branch created or switched (from the reflog), tag or release, revert (linked to the commit it undid), rewritten history (amend, rebase).

The agent mark sits beside the icon: the icon says what happened, the mark says who.

## Opening it

- Palette **Review**, a shortcut, `compi review`: every project.
- From inside a project: that project, with an **All projects** switch.
- Terminal first: opens in place of the focused terminal, like Monitor. Chat first: opens beside the focused chat.

## Which agent made a commit

Three sources, most certain first:

1. **Chat:** the native chat whose tool call ran `git commit`.
2. **Terminal:** Compi watches each project's history; when a commit appears, the terminal in that repository that had an agent running at that moment (`claude`, `codex`, …) gets the credit. Two agents in one repository at once is rare; if it happens, a chat tool call or the trailer decides, else no mark.
3. **`Co-Authored-By` trailer:** for commits made outside Compi.

A commit from a Compi terminal with no agent running shows **you**. Anything else unmatched shows no mark; the view never guesses. Per-turn file snapshots are not needed for this list.

`compi review --json` gives agents the same list with credit, so an agent in a terminal can ask what is unreviewed or summarize its own work.

## Diff view additions

- A diff can cover a range: several commits plus uncommitted work.
- Line comments (if they prove useful): a comment on a line goes to the thread whose commit wrote it.

## References

- T3 Code: per-turn checkpoints as hidden refs; diff scopes (turn, thread, commit, branch, uncommitted); comments as composer chips. See `docs/research/t3-code/`.
- Delta (Zed): Changes comparisons (Last Turn, Last Commit, upstream); per-file Seen with progress; comments pending until the next message; review threads with a verdict.
- Codex app: review pane scopes including Last turn; **+** on a line to comment.
- Warp: Code Review panel (uncommitted, vs main); batched comments sent to any CLI agent.
- Factory: session diff with line comments; `/review` presets.
- Herdr: basic Changes panel only; strong all-agents status list.

### Visual references

- Warp, comments batched and sent to the agent: <https://docs.warp.dev/_astro/interactive-code-review-batch-comments.CDOsIJLb_Z1mgtz5.webp>; panel docs: <https://docs.warp.dev/code/code-review/>
- Cursor Agents Window, the agent's summary with its checks (✅/⚠️ per command) beside the file: <https://cursor.com/docs-static/images/agent/file-agents-window-final.png>; docs: <https://cursor.com/docs/agent/agents-window>
- Agent session dashboard, cards grouped by machine (repo, first prompt, branch, status, time): <https://blog.marcnuri.com/assets/dashboard-overview.CPrlzN7Y_1DimYf.png>; post: <https://blog.marcnuri.com/ai-coding-agent-dashboard>
- Murmur dashboard, turns folded to one line with an artifacts card (commits, PRs, files): <https://docs.murmur.dev/guides/dashboard>
- Delta, Changes comparisons, Seen progress and review verdicts: <https://delta.dev/docs/agents/review-and-sync>; comments: <https://delta.dev/docs/agents/comments>
- Codex app review pane: <https://developers.openai.com/codex/app/review>

## Decided

- No docs-only or tests-only badge: the language icon already shows it.
- Ranges: since review (default), or from a chosen commit or tag.

## Settled details

- Look: mock-ups beside this note (Compi's own icons; colours approximate): `review-view-mock.png` (first: commit rows with +/−), `review-view-mock-tasks.png` (tasks with Asked / Did, areas, flags, commits folded under), `review-view-mock-commits.png` (commits with Asked / Did underneath).
- Rows are **tasks** (chosen 2026-11-25, `review-view-mock-tasks.png`): one card per thread's work since review, with **Goal · Result · Checks** (wording from Warp's Goal / Validation and Cursor's Summary / Testing; chosen 2026-11-25), areas, flags and its commits folded under it. Your own commits show their commit body as the Result.
- Flags are general: new files, deleted files, dependencies, CI, tests added, version.
- Checks: the last run of each test, build or lint command in those turns, passed or failed.
- Goal: the agent's own restatement at the start of the turn ("I'll …"); falls back to your message cut to one line (chosen 2026-11-27).
- Clicks (chosen 2026-11-27): the card opens one diff of all its commits in the existing diff view; its title jumps to the thread (chat or terminal) at the turn that committed; a check opens that command's output; an area chip filters the diff to that folder; a commit row opens that commit alone; **Mark reviewed** on a card clears that card, on the project line the whole project.
- Working threads show as soon as their first commit lands, marked **Working**; Result fills in when the turn ends, and **Mark reviewed** stays off until the thread is Done (decided 2026-11-27).
- The "reviewed up to" point is kept per machine, in the daemon's state, like other workspace data. No cloud.
- No new icon in the top strip: palette, shortcut, `compi review`, and the project heading's menu open it.

No open questions remain; the design is ready to plan.
