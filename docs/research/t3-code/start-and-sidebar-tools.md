# T3 Code reference: new-thread screen and sidebar search and rows

T3 does not create a thread until the first send. Before that, `/draft/$draftId` shows a centred "hero" with a project heading over the composer and a context strip. The send creates the thread on the server and starts its first turn in one call. The route then swaps to the thread URL without remounting the view. The sidebar search is an inline filter at the top of the sidebar. It matches titles, branches and PRs first, then message text. Picking a message match opens in-thread find on that message.

## 1. The new-thread (draft) screen

**What it shows** (`apps/web/src/components/ChatView.tsx:3600-3660`, `3825-3910`). While `isDraftHeroState` is true, the composer sits vertically centred. Above it is `DraftHeroHeadline`, which shows:
- **Project heading picker:** a combobox with search, "Add project", per-project pin, and an environment badge when projects span machines (`chat/DraftHeroHeadline.tsx:205-298`). Its states (`:352-376`):
  - "**Project** to start" when no project is chosen yet.
  - "Add a project to start" when there are no projects.
  - "What should we work on?" for a draft with no project.
- **"or start without a project"** link, with `mod+alt+n` (`chat.newWithoutProject`) shown in its tooltip (`:323-345`).

Under the composer is the **context strip** (`BranchToolbar`). It always shows for a new thread with a project, "since it names the permissions the first send uses" (`BranchToolbar.logic.ts:72-85`). It holds:
- env mode (local or New worktree) and base branch, plus start-from-origin;
- machine picker or Auto balance;
- PR checkout;
- permissions.

Notices go out of flow below the composer, so the composer does not move (`ChatView.tsx:3893`). I found **no suggestions and no recent-threads list** on this screen in the source [INFERENCE: absent]. The model and host pickers live in the composer (`mod+shift+m`, `mod+shift+h`; `docs/user/keybindings.md:33`).

**What is set where.**
- Model and mode are sticky across new threads, unless the project has its own default model (`docs/user/composer.md:123-125`, `thread-sidebar.md:7-13`).
- Branch and workspace mode come from your configured defaults (`thread-sidebar.md:7-13`).
- The machine can change only while the thread is an unsent draft (`ChatView.tsx:1315-1326`). After that, `isThreadEnvironmentLocked` locks it.
- Picking another project on an *empty* draft opens that project's own draft. On a draft with content, it retargets the same draft and re-seeds the model (`DraftHeroHeadline.tsx:171-182`).

**Draft identity and reuse** (`hooks/useHandleNewThread.ts:225-445`):
- Each logical project has at most one stored draft.
- Choosing "New thread" while that draft is empty reuses it instead of making another.
- Otherwise it mints a new `draftId` plus a client-side `threadId` and navigates to `/draft/$draftId`.
- Visiting `/` automatically opens a draft in the most recently used project, replacing `/` in history (`routes/_chat.index.tsx:55-75`).

**When the thread is created: on the first send, not while typing.**
- `useComposerSend.ts:1329-1394` sends `startThreadTurn` with `bootstrap.createThread` (project, title, model, runtimeMode, branch, worktreePath, createdAt).
- When a worktree is involved it also sends `prepareWorktree` and `runSetupScript`.
- If the send fails, the draft is restored (`restoreFailedDraftThread`) and its text goes back into the composer (`:1475-1500`).

**Transition into the conversation.**
- On a foreground send, before dispatching, the composer's position is recorded (`captureDraftHeroComposerRect`) and the hero is marked "docked" (`useComposerSend.ts:957-972`).
- `resolveDraftHeroState` then becomes false (`ChatView.logic.ts:323-344`).
- A slide animation (FLIP) moves the composer from the centre to the bottom dock (`chat/useDraftHeroLayoutTransition.ts`), and the headline unmounts.
- The draft is marked `promotedTo`. `ThreadRouteView` renders `ChatView` with the **same key** and `<Navigate replace>` to `/$environmentId/$threadId`, so the swap keeps the instance (`ThreadRouteView.tsx:193-207`).
- I did not trace the focus code beyond the stable composer instance. [INFERENCE] The composer keeps focus because it never remounts.

**Navigating away from an unsent draft.**
- The draft persists in `composerDraftStore`.
- `SidebarDraftBlock` lists every unpromoted draft that has user content, newest first, above a divider (`sidebar/SidebarDraftBlock.tsx:192-245`).
- A row shows a warning-tinted pen, the project favicon and name, the first line of the prompt (or "N attachments"), and an **×** on hover that discards it (`:100-146`).
- Discard has Undo for 5 s (`thread-sidebar.md:66-68`).
- The draft you are currently on is "frozen": it appears only if it already had content when you arrived on it (`:178-191`). In practice a draft enters the sidebar once you leave it with text in it.
- Existing threads with an unsent reply get the same pen icon and **Discard draft** button (`SidebarThreadRow.tsx:1251-1270`, `1614-1633`).

**Background send** (`docs/user/keybindings.md:20-25`; `useComposerSend.ts:1359-1460`).
- In a new thread, `mod+Enter` (or `mod+Alt+Enter` anywhere) starts the thread in the background.
- It immediately opens a fresh draft with the same env mode and base branch (`resolveBackgroundDraftWorkspaceOptions`).
- The hero is not docked.
- A toast "Started in background" offers **Open**. On failure the toast reads "Background task failed" and offers **Open draft**.
- With New worktree, each background send gets its own worktree.
- Shift-clicking several models sends one thread per model while you stay in the composer (`thread-sidebar.md:46-58`).

## 2. Sidebar search

**Input.** The search field sits at the top of the sidebar next to the project-scope, Add project and New thread buttons. It is a combobox with ARIA active-descendant and an **×** that clears and refocuses (`sidebar/SidebarThreadHeader.tsx:63-98`). I found no dedicated focus shortcut for it [INFERENCE]. `Cmd/Ctrl+K` opens the separate command palette, which uses the same ranking.

**What it matches.** `searchSidebarThreads` (`Sidebar.logic.ts:1100-1137`) returns two groups:
- **Title, branch and PR matches**, ranked like the palette, with no boost for previously picked results.
- **Content matches**: threads listed only because a server message search hit (`useThreadSearch`), in server rank order, de-duplicated against the first group.

Search respects the project-scope filter (`Sidebar.tsx:1196-1202`). Message search needs a word of 3 or more letters (2 for Chinese, Japanese and Korean) (`thread-sidebar.md:205-212`). Rows already shown keep their places while slower environments answer (`Sidebar.tsx:1365-1400`).

**Results list** (`Sidebar.tsx:3220-3305`; `SidebarSearchResultRow`, `SidebarThreadRow.tsx:1720-1901`). Each result shows:
- favicon (greyed if settled);
- title;
- matching PR number;
- relative time;
- for content matches, a snippet excerpt with the query highlighted.

Results show the full hover card and can be dragged out as chat context or have files dropped on them.

**Keys and behaviour** (`Sidebar.tsx:1505-1540`):
- ↑/↓ wrap around the list.
- The highlight follows its thread, not its index.
- Enter or click clears the query, navigates, and for a content match calls `openThreadFindOnQuery` to **jump to the matching message**.
- Esc clears the search.
- Focus stays in the input.

**Empty state.** It shows "Searching thread messages…", then "Couldn't search thread messages", or "No threads found".

## 3. Thread row anatomy (card variant, `SidebarThreadRow.tsx:1504-1695`)

**Line 1**, left to right:
- unsent-draft pen;
- project favicon;
- pin (it turns into **unpin** on hover);
- project name;
- **status slot**, which becomes hover actions.

**Status slot** (`:770-840`, `resolveSidebarThreadStatus` `Sidebar.logic.ts:925-959`). An icon and label in the status colour:

| Status | Colour |
|---|---|
| Working / Setting up / Goal (with live duration) | info |
| Waiting (with duration) | muted |
| Approval | warning |
| Input | indigo |
| Request (guest message waiting on you) | guest |
| Limited (usage limit) | warning |
| Failed | error |
| Woke (from snooze; the label is a dismiss button) | warning |
| Done (finished and you haven't opened it since) | success |

- With no status, the slot shows a compact relative time.
- A failure you have already opened goes back to ready (`:961-968`).
- Idle rows that are working, waiting, or already seen "recede": muted text and normal font weight (`:894-916`).

**Hover actions** replace the status: Discard draft ×, Snooze (clock menu with presets and Custom…), **✓ Settle**. Settle, Un-settle and Wake can be pressed and dragged to apply to a range of rows. A jump-hint key badge (mod+1…9) can take over the slot.

**Line 2:** the title. Double-click or F2 renames it in place: Enter or blur commits, Esc cancels, an empty title shows a warning toast, and an unchanged title is a no-op (`:860-893`; `Sidebar.tsx:1542-1560`). While a title is regenerating it shimmers.

**Line 3** (meta; omitted when empty, with its icons moving up to the title line):
- worktree folder icon, if in a worktree, with the path in its tooltip;
- middle-truncated branch, or the machine label for no-project threads;
- **terminal icon**: teal and pulsing while a process runs; clicking it opens that terminal (`ThreadStatusIndicators.tsx:698-770`);
- **PR badge**: state-coloured icon and #number — green open, grey draft, red closed, violet merged (`pullRequest/pullRequestIcons.tsx:35-55`). A "+N" or stack variant covers several or stacked PRs. Click opens the PR, in the right panel if configured;
- remote-machine icon;
- provider stack: past providers in greyscale, the current one on top.

**Hover card:** project, machine, branch, a warning when your local checkout is on another branch, model · provider, "Handed off from…", terminal count, and the last error.

**Other row states:** active row, selection, unsent-draft tint, file-drop ring, and drag destination badges (Pin, Unpin, Settle, Un-settle, Wake).

## Implications for Compi
- Keep "draft" as a client-only object: draft id plus a client-minted thread id, at most one per project, reused while empty. Create the server thread **atomically on first send** (create, optional worktree, first turn) and restore the draft text if that fails.
- Swap the draft view into the thread view without remounting it (keep the same entity key), and animate the composer from centre to dock. That gives a "Chat-first" start without losing focus.
- Put all pre-send choices (project, machine, worktree/branch, permissions, model) on the new-thread screen, and lock the machine after the first message.
- List unsent drafts with content in the sidebar above the threads, with a distinct tint or icon and a discard button with Undo. Mark existing threads that have unsent replies the same way.
- Make sidebar search an inline filter in two tiers: metadata matches, then message matches with a snippet. Enter jumps into in-thread find at the hit, and focus stays in the input.
- Give each row one status slot with strict priority (approval > input > request > working > failed > woke > done > time) that turns into Snooze and Settle actions on hover, and fade rows that are idle or already seen.
