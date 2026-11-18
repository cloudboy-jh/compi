# T3 Code: projects/threads sidebar and how terminals relate to threads

## 1. Object model
- **Environment**: the machine running a T3 server (local, remote or WSL). Every thread is identified by its environment plus its thread id (`apps/web/src/components/Sidebar.tsx:1456-1468`). Settings are written to each selected environment (one, or **All environments**); offline environments keep their old values (`docs/user/project-settings.md:3-30`).
- **Project**: a folder on one environment, grouped across machines and checkouts into a "project group" (`docs/user/project-settings.md:145-148`). It can be pinned (comes first in every picker) or archived (leaves the pickers but keeps its threads) (`docs/user/project-settings.md:52-67`). A thread can have **No project**: it then works in its own folder under `~/.t3/scratch`, and **Turn into project** promotes that folder in place (`docs/user/thread-sidebar.md:21-46`).
- **Thread**: one conversation. It records a branch and optionally a worktree path, and has three independent states: pinned, settled and snoozed (`apps/web/src/components/Sidebar.logic.ts:196-207`).
- **Worktree**: chosen per thread when it starts (**New worktree** or the current checkout). **New thread in this worktree** reuses an existing one (`docs/user/thread-sidebar.md:3-11`; `Sidebar.tsx:2722-2729`). The row shows a worktree icon with a tooltip giving path and branch (`apps/web/src/components/ThreadStatusIndicators.tsx:776-806`).

## 2. Adding projects
- When there are no projects, the sidebar shows an **Add project** button that opens the command palette (`Sidebar.tsx:3317-3331`).
- The palette asks for an environment first, then a source (`apps/web/src/components/CommandPalette.tsx:1080-1175`):
  - **New project**: `git init` from a name.
  - **Local folder**: browse a folder on disk.
  - **Git URL**: clone from a URL.
  - Provider repositories (e.g. GitHub). Sources that aren't configured stay listed but disabled, with a **Setup Required** button.
- These items use `keepOpen`, so the palette stays open as a multi-step wizard.

## 3. Sections, ordering, filtering
- **Sections, top to bottom** (`Sidebar.logic.ts:209-216`):
  - Pinned, Active
  - Working (folded, beta, on by default)
  - Snoozed, Settled (collapsible shelves)
- **Ordering**:
  - New threads go to the top of Active. Thread activity never reorders the list. Manual order is saved on the server and syncs across devices (`docs/user/thread-sidebar.md:121-125`).
  - When the Working section is on, Active is ordered by "last came back to you" and can't be reordered.
  - Settings also have sort orders for projects (`updated_at`/`created_at`/`manual`) and threads (`updated_at`/`created_at`) (`packages/contracts/src/settings.ts:75-81`).
- **Project filter** (multi-select combobox in the header, `Sidebar.tsx:2996-3127`):
  - Click selects one project; **Shift-click** or **Shift+Enter** toggles more.
  - The `mod+shift+p` pin shortcut pins the highlighted project.
  - **ContextMenu** or **Shift+F10** opens that project's settings.
  - If the filter hides everything, the list says so and offers **Show all projects** (`Sidebar.tsx:3333-3359`).
- **Group by project** (Labs, "boring sidebar"): one folder per project. A closed folder shows its thread count and one dot for the most urgent hidden state, in this order: attention, then failed, then unread, then working (`Sidebar.logic.ts:996-1022`).
- **Search**: the sidebar search box matches titles and message text. ↑/↓ move the highlight, Enter opens the result (jumping to the matching message), Esc clears. Focus stays in the search box (`Sidebar.tsx:1508-1532`).

## 4. Status indicators
Status priority (`Sidebar.logic.ts:1190-1280`):

$$\text{Pending Approval} > \text{Awaiting Input} > \text{Guest Request} > \text{Working} > \text{Connecting} > \text{Waiting} > \text{Failed} > \text{Plan Ready} > \text{Completed}$$

- Only **Working** and **Connecting** pulse.
- **Completed** (unread) means the latest run finished after the server-side `lastVisitedAt`. Interrupted or cancelled runs don't count (`Sidebar.logic.ts:762-785`).
- **Mark unread** rewinds that watermark.
- **Failed** clears once you've opened the thread.
- Idle rows fade into the background, but a row never fades while it is active, selected, or waiting on you (`Sidebar.logic.ts:897-917`).
- A separate pulsing terminal icon means a terminal process is running. Clicking it opens and focuses that terminal and navigates to the thread (`ThreadStatusIndicators.tsx:698-770`).

## 5. Row interactions
- **Click** opens the thread and clears any multi-selection.
- **Mod-click** toggles a row in the selection; **Shift-click** selects a range across the visible list (`Sidebar.tsx:1577-1596`).
- The second click of a double-click doesn't navigate again (`Sidebar.logic.ts:787-790`).
- **Rename**: double-click the title or press **F2** (`apps/web/src/components/sidebar/SidebarThreadRow.tsx:900-925`).
  - The input auto-selects its text.
  - Enter commits, Esc cancels, and clicking away (blur) also commits.
  - An empty title is rejected with a toast; an unchanged title is ignored.
  - The chat header title supports the same rename (`apps/web/src/components/chat/ChatHeader.tsx:166-247`).
- **Context menu** (`Sidebar.tsx:2600-2890`):
  - Filter by project, project settings
  - New thread on this branch/worktree
  - Settle / un-settle, snooze (presets or custom), pin / unpin, auto-settle Never/Follow
  - Rename, regenerate title, mark unread
  - Copy path / branch / thread / thread ID
  - Share with guest, turn into project
  - Archive, delete
- **Multi-select context menu**: bulk settle, snooze, pin, regenerate titles, mark unread and delete, with counts in the labels (`Sidebar.tsx:2300-2560`).
- **Confirmations** (settings): delete is on by default, archive and unpin are off by default (`packages/contracts/src/settings.ts:471-474`). Delete warns that conversation history will be cleared permanently.
- **Undo**: unpin, settle, snooze, archive and discarding a draft show a 5-second **Undo** notice. `mod+z` works even from the composer until you type, and consecutive actions of the same kind undo together (`docs/user/thread-sidebar.md:65-71`; `docs/user/keybindings.md:180-185`).
- **Moving away from the current thread**: settling or snoozing the open thread moves you to the next live thread; if none is left you land on a new draft in that project (`Sidebar.tsx:1599-1622`).
- **Dropping files** onto a row opens that thread and attaches the files to its composer.

## 6. Drag behavior
- **Drag between sections** to pin, unpin, settle, un-settle or wake (`Sidebar.logic.ts:304-314`).
- **What appears while dragging**:
  - The dragged row shows the action label ("Pin", "Settle", …).
  - Other rows slide aside to show the gap, and section labels appear only during the drag.
  - You can't drop into Snoozed or Working.
- **Sweep**: press **Settle**, **Un-settle** or **Wake** on one row and drag up or down to apply it to every row in between; Esc cancels (`Sidebar.logic.ts:316-338`; `docs/user/thread-sidebar.md:154-156`).
- **Scroll position**: pinning or unpinning keeps your scroll position instead of following the row. Animations respect reduced-motion.

## 7. Keyboard
- `mod+1`–`9`: jump to the first nine visible threads (desktop only). Number hints appear after holding the modifier for 200 ms (`Sidebar.logic.ts:75`, `:743-760`).
- `thread.previous` / `thread.next`: move through the visible list in order (`Sidebar.tsx:2908-2930`).
- Thread commands: `thread.pin`, `thread.settle`, `thread.undo`, `thread.regenerateTitle`, `thread.stop`.
- Navigation and other: `sidebar.toggle`, `mod+[` / `mod+]` (back/forward), `Cmd/Ctrl+K` (palette search across environments).
- New threads: `chat.new` (may show the project picker), `chat.newLocal` (current project), `chat.newWithoutProject` (`mod+alt+n`) (`docs/user/keybindings.md:174-195`; `packages/contracts/src/keybindings.ts:38-108`).
- Shortcuts don't fire while the command palette or model picker is open, or on key-repeat. Most defaults include `!terminalFocus` so they don't steal terminal input.

## 8. New-thread flow
1. **New thread** opens a "New thread in…" project picker when more than one project is open. **Shift-click** uses the current project instead (`Sidebar.logic.ts:802-809`; `Sidebar.tsx:2948-2970`).
2. A draft appears and is listed in the sidebar. Its heading lets you change the project or pick **No project**; changing the project keeps the same environment when that project exists there.
3. It carries over your model and mode, unless the project has its own model default. Branch and worktree mode come from your defaults.
4. Pickers and shortcuts:
   - `mod+shift+m` model, `mod+shift+h` host, `mod+shift+x` workspace, `mod+shift+g` branch, `mod+shift+l` reuse the previous worktree.
   - **Shift-click** several models to start one thread and one worktree per model.
5. `mod+Enter` in a new thread sends it in the background and opens a fresh draft with the same workspace mode and base branch (`docs/user/thread-sidebar.md:48-59`; `docs/user/keybindings.md:20-36`).

## 9. Terminals and threads
- **Ownership**: each thread has its own terminal drawer (open flag, height default 280, groups, active terminal), stored per thread (`apps/web/src/terminalUiStateStore.ts:14-18`).
- **Working directory**: the thread's worktree, otherwise the project root, with matching runtime env (`apps/web/src/components/PersistentThreadTerminals.tsx:179-205`).
- **Keys**: `mod+g` toggles the drawer, `terminal.new`, `terminal.split`/`splitVertical`, and `mod+w` closes the focused terminal but never the window (`docs/user/keybindings.md:104-105`, `:198-202`).
- **Groups and splits**:
  - Up to 4 terminals per split group (`apps/web/src/types.ts:36`).
  - A new split goes right after the active terminal.
  - Closing the active one focuses its neighbor in the group; closing the last one resets the drawer (`terminalUiStateStore.ts:400-440`).
- **Persistence**:
  - Drawers for up to 10 hidden threads stay mounted, so switching back finds them as you left them (`apps/web/src/components/chat/ChatTerminalDrawers.tsx:36-90`; `apps/web/src/components/ChatView.logic.ts:114`).
  - The client's terminal list is reconciled with the server's list (`PersistentThreadTerminals.tsx:170-178`).
  - The server keeps 5,000 lines / 8 MiB of scrollback per terminal (`docs/user/terminal.md:31-37`).
- **On settle**: terminals sitting at an idle prompt close and keep their output; running commands stay unless the action has `stopOnSettle` (`docs/user/thread-sidebar.md:136-152`).
- **Terminals in the right panel**: if the terminal lives in a right-panel tab, "reveal" activates that tab instead of the drawer (`apps/web/src/lib/revealThreadTerminal.ts:7-24`).
- **Selected terminal text** can be added to the composer as context.

## 10. Chat header
- **Breadcrumb**: project (icon + name) / parent thread / title.
  - Clicking the project name starts a new thread in that project.
  - The parent thread's title collapses to an ellipsis when space is short.
  - Clicking the title opens the thread menu; double-clicking it renames.
  - Right-click opens the thread menu (or Project settings for a draft) (`ChatHeader.tsx:198-300`).

## 11. Settings that change sidebar behavior
- Working section (`sidebarWorkingShelfEnabled`)
- Boring sidebar / group by project (+ multiline rows)
- Project and thread sort orders
- Delete / archive / unpin confirmations
- Use last project (mobile)
- Per-environment auto-settle: 7 days inactive, on merge, keep threads with a PR (`packages/contracts/src/settings.ts:530-560`, `:1454-1490`)
- Snooze presets
- Title instructions

## Takeaways for Compi
- The list never reorders on activity.
- Every state change is a drag or a menu item, and the reversible ones get a 5 s undo.
- Unread is based on the server's last-visited time, not on client-side checks.
- Terminals belong to the thread and run in its worktree, and they stay alive while you switch threads.
