# T3 Code file picker, file explorer, and chat/thread/project settings

## Summary
- **The picker is a command-palette overlay, not part of the sidebar.** `mod+p` opens it (`filePicker.toggle`, `when: "!terminalFocus"`, `packages/shared/src/keybindings.ts:87`). It reuses `CommandPaletteContent` in `"files"` mode (`apps/web/src/components/CommandPalette.logic.ts:918`, `apps/web/src/shortcuts/commandPaletteCommands.ts:103`).
- **The tree lives in the right panel.** Its tab is the `files` surface, and it shows beside open files (`apps/web/src/rightPanelStore.ts:53-62`).
- **Both search the thread's checkout:** `thread.worktreePath ?? project.workspaceRoot` (`apps/web/src/hooks/useActiveProjectTarget.ts:26`).
- **Indexing and search run on the server**, using the native `fff` FileFinder.

## Files
- `apps/web/src/components/files/ProjectFilePicker.tsx`: the picker UI, highlighting, and Enter → `openFile`.
- `apps/web/src/components/files/ProjectFilePicker.logic.ts:4-63`: the 200-result cap, query normalisation, and highlight indices.
- `apps/web/src/components/files/projectFilesQueryState.ts:216-242`: the picker query. Empty queries are allowed and come back frecency-ranked.
- `apps/web/src/state/queries.ts:50,307-358`: 120 ms debounce, query cut to 256 chars.
- `apps/server/src/workspace/WorkspaceSearchIndex.ts:52-57,563-569,1231-1294`: index constants, `FileFinder.create`, search.
- `packages/contracts/src/project.ts:19,241-302`: RPC schemas. `limit ≤ 200`; `kind` and `imageOnly` are optional.
- `apps/web/src/components/files/FileBrowserPanel.tsx`: the tree (Pierre `@pierre/trees`).
- `apps/web/src/components/files/FileExplorerPane.tsx`: resizable tree column.
- `apps/web/src/components/files/FilePreviewPanel.tsx:1255-1820`: preview, edit, and explorer layout.
- `apps/web/src/components/files/FileBreadcrumbs.tsx`: per-folder crumb menus.
- `packages/contracts/src/settings.ts`: all setting defaults.

## Facts

### File picker
- **Scope:** files only (`kind: "file"`) in the active thread's cwd. With no thread or project it shows "Open a project to search its files." with the input disabled (`ProjectFilePicker.tsx:63-80`).
- **Ranking:** fuzzy ranking from `fff` on the server. The client keeps that order (`ProjectFilePicker.logic.ts:35`). Empty queries come back frecency-ranked (`projectFilesQueryState.ts:216`); the index tracks access and modification frecency (`WorkspaceSearchIndex.test.ts:38-40`).
- **Query handling:**
  - Server: `[` is stripped from the query, and `{` is retried without it if the first search finds nothing (`WorkspaceSearchIndex.ts:1281-1291`).
  - Client: leading `@./` and whitespace are trimmed only to compute highlights (`logic.ts:42-44`).
- **Index source:** `FileFinder.create({basePath: cwd, disableContentIndexing, aiMode:false})` (`:563-569`). It holds at most 25 000 entries, gives up scanning after 15 s, and is dropped after 15 min idle (`:52-57`). It refreshes after agent runs and file writes (`orchestration-v2/RunFinalizationService.ts:138`, `WorkspaceFileSystem.ts:640`).
- **Gitignore:** `fff` applies `.gitignore`. The server adds `.git/info/exclude` and the global excludes via `git … --ignored`, and evaluates `.ignore` files ripgrep-style, nearest-first (`:443-503, 657-669`). Ignored paths do not appear in search results.
- **Limits:** the picker shows 200 results; the RPC caps `limit` at 200 and queries at 256 chars.
- **Highlight:** a client-side subsequence match runs on both the basename and the full path, and matched characters render bold. A path the server matched but the client can't match shows with no highlight (`ProjectFilePicker.tsx:26-46`, `logic.ts:17-33`).
- **Keys:** Up/Down plus Enter. The first row is always highlighted (`autoHighlight="always"`). Escape is labelled "Back". The empty state reads "Indexing…" or "Searching…".
- **Enter:** closes the picker and calls `rightPanelStore.openFile(threadRef, path)` (`ProjectFilePicker.tsx:118-120,155-158`). I found no modifier variants (no matches for `metaKey`/`ctrlKey`/`shiftKey` in the palette components).

### Explorer tree
- **Where it lives:** the right-panel `files` tab, opened via `useRightPanelActions.ts:146`. When a file is open, the tree sits as a resizable column next to it: 256 px default, 160–560 px, at least 256 px kept for content, with a width storage key (`FileExplorerPane.tsx:9-21`). The resize handle accepts ←/→ in 16 px steps plus Home/End.
- **Toggle:** a "Hide/Show file explorer" toggle (`FilePreviewPanel.tsx:1562`). The tree is hidden for attachments and host paths (`filePreviewMode.ts:42-51`).
- **Loading:** folders start collapsed (`initialExpansion: "closed"`) and each folder is listed lazily when expanded (`useDirectoryEntries`). The list includes ignored entries flagged `ignored: true` and dimmed. `.git` is excluded (`WorkspaceEntries.test.ts:110-150`).
- **Expanded state** is persisted per `env:cwd` (`FileBrowserPanel.tsx:358-399`).
- **Expand all / collapse all** skips ignored folders (`:154-157,324-333`).
- **Search:** `mod+f` focuses the tree's search box. Results come from the server path search, limit 200, and the tree hides non-matching rows. Escape clears it. The tree also shows "More matches available" and a refresh button (`:279, 230-235, 524-531`).
- **Selecting a file row opens it** (`onSelectionChange` → `onOpenFile`, `:282-287`). Enter or click then moves focus to the editor (`FilePreviewPanel.tsx:1789-1792`).
- **Right-click** gives the OS file menu plus "Copy mention" and "Add to chat". The latter inserts an `@file` link into the composer. Dragging a row into the composer also inserts a mention.
- **External opens** (picker, links) reveal the file in the tree: ancestors expand, the row is selected and scrolled to the center (`:478-535`).

### Preview vs. edit
- **`openFile` behaviour:** each file gets its own tab (`file:<path>`), and opening it removes the standalone `files` tab. Opening a path that already has a tab bumps `revealRequestId` (`rightPanelStore.ts:916-951`).
- **When files are editable:** text files are editable inline unless they are truncated, host files, or the connection can't write, in which case they are read-only (`FilePreviewPanel.tsx:1746-1777`).
- **Saving:** autosave runs on a 500 ms debounce (`fileSaveSessions.ts:140`).
- **Rendered vs. source views:**
  - Markdown, Mermaid, HTML, CSV and other tables toggle between rendered and source. Rendered Markdown defaults off; HTML and tables default to rendered (`:1261-1273`).
  - Markdown task-list checkboxes stay editable in the rendered view.
- **Other file types:** media, PDF, Office documents, and fonts get dedicated viewers.
- **Breadcrumbs:** each crumb opens a folder menu. Pressing ← in that menu goes up a level.

## Settings and defaults

**Client (Settings → Chat and Appearance; `settings.ts:476-600`)**

| Setting | Default |
|---|---|
| `sendShortcut` (`enter` / `mod-enter-multiline` / `mod-enter`) | `enter` |
| `followUpBehavior` (`queue` / `steer`) | `steer`; `mod+Enter` does the opposite for one message |
| `composerRichTextEnabled` | `true` |
| `showSkillsInSlashMenu` | `true` |
| `toolCallVisibility` | `showAll` |
| `proactivePanelsEnabled` | `false` |
| `persistComposerContextStrip` | `false` |
| `chatWidth` | `comfortable` |
| `timestampFormat` | `locale` |
| `wordWrap` | `true` |
| `mathRenderingEnabled` | `false` |
| `boringSidebarEnabled` | `false` |
| `sidebarWorkingShelfEnabled` | `true` |
| `sidebarProjectGroupingMode` | `repository` |
| `sidebarProjectSortOrder` | `updated_at` |
| `sidebarThreadSortOrder` | `updated_at` |
| `confirmThreadDelete` | `true` |
| `confirmThreadArchive` | `false` |

**Server (Settings → Threads; can be overridden per project, `settings.ts:1175-1203, 1384-1535`)**

| Setting | Default |
|---|---|
| `defaultModelSelection` | `null` ("Automatic") |
| `fallbackModelSelection` | `null`; used when the default provider runs out of usage |
| `defaultRuntimeMode` (permissions: `approval-required` / `auto-accept-edits` / `auto` / `full-access`) | `full-access` (`providerPolicy.ts:25-32`) |
| `defaultThreadEnvMode` (`local` / `worktree`) | `null`: inherits `t3.json`, then `local` |
| `responseStreamingMode` | `paragraph` |
| `textGenerationModelSelection` | codex `gpt-6-luna` |
| `sidebarAutoSettleAfterDays` | `7` (range 1–90) |
| `sidebarAutoSettleOnMerge` | `true` |
| `continueThreadsAfterServerUpdate` | `true` |

- **Per-project provider enable/disable** is also available (`docs/user/project-settings.md`, "Settings and project overrides").
- **Precedence:** project override > environment > `t3.json` > built-in default (`project-settings.md`, "Defaults and inheritance").
- **Picker-related keys:** `mod+shift+m` opens the model picker and `mod+shift+a` the permissions picker (`docs/user/keybindings.md`).

## Implications for Compi
- **Index on the daemon.** Build one gitignore-aware fuzzy index per *thread cwd* (worktree first, then project root). Key it by cwd, evict it when idle, and refresh it after agent turns and writes. A Rust crate such as `ignore` + `nucleo` fits [INFERENCE].
- **Picker IPC contract:** send `{cwd, query≤256, limit≤200, kind}` and return `{entries[{path, kind, ignored?}], truncated}`. Debounce about 120 ms on the client. An empty query should return recent/frecent files.
- **Two kinds of folder listing.** The tree lists one folder at a time, lazily, and includes ignored entries flagged `ignored`. Search excludes ignored paths. Expand-all should skip ignored folders.
- **Placement:** T3 keeps the picker in a palette and the tree in the right panel. If Compi puts the picker in the sidebar, keep the same scope (the active thread's checkout). Opening a file should go to a per-file tab and reveal it in the tree.
- **Settings ownership.** Server-side, overridable per project: model, permissions (`full-access` default), workspace mode, streaming. Device-side: send shortcut (`enter`) and follow-up behavior (`steer`). Compi's Chat-first/Terminal-first setting fits in the client tier [INFERENCE].
- **Editing:** T3 edits files inline with a 500 ms autosave and falls back to read-only previews. Compi needs to decide whether its first version edits at all [INFERENCE].
