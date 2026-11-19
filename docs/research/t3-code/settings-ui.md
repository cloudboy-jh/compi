# T3 Code settings screens: structure, look and behaviour

Paths below start at `apps/web/src/` unless they begin with `docs/`, `packages/` or `apps/desktop/`. Setting defaults are already listed in [Files and settings](files-settings.md#settings-and-defaults); this report covers the screens that edit them.

## Summary
- **Settings is a large modal laid over the app, but it is a route.** `/settings/<section>` renders inside `AppRouteModal`. Esc, the close button or a click on the backdrop goes back to the page it opened over (`components/AppRouteModal.tsx:13-17`, `routes/_chat.settings.tsx:136-158`).
- **The layout is a two-pane page.** On the left is a 240 px section list with search; on the right a 48 px header with the breadcrumb "Settings / Section" and a close button, then a scrolling page of rounded cards (`components/settings/SettingsSidebarNav.tsx:258-266`, `routes/_chat.settings.tsx:140-150`).
- **Every row is the same component.** `SettingsRow` puts title and description on the left and the control on the right. Beside the title sit two 20 px slots: an inheritance marker (layers icon) and a reset button (`components/settings/settingsLayout.tsx:313-583`).
- **Scope is a sentence at the top of the page:** "Applying settings for [project ▾] across [environment ▾]". Picking a project turns the same rows into project overrides (`components/settings/SettingsScopeSentence.tsx:71-146`, `docs/user/project-settings.md:1-30`).
- **Changes save at once,** with no Save button, no debounce and no undo. Selects and switches save on change; text fields save on blur or Enter. A failed save shows an error toast (`components/settings/useScopedSettings.ts:61-154`).

## 1. Structure

### How it opens
- **Ways to open it:**
  - The Settings item in the sidebar footer (`components/sidebar/SidebarChrome.tsx:254-257,285-287`).
  - "Open settings" in the command palette (`components/CommandPalette.tsx:1764-1774`).
  - The desktop menu item "Settings..." with `CmdOrCtrl+,` (`apps/desktop/src/window/DesktopApplicationMenu.ts:227-231,251-255`). It reaches the web app as the `open-settings` menu action (`components/AppSidebarLayout.tsx:306-309`).
- **`/settings` redirects to `/settings/general`.** Old section links are redirected to where their rows now live (`routes/_chat.settings.tsx:193-199`, `components/settings/settingsSearch.ts:1509-1519`).
- **Project settings is a deep link.** Several places navigate to `/projects/$projectKey`, which redirects to `/settings/projects?project=<key>`:
  - the sidebar project menu item "Project settings" (`components/Sidebar.tsx:1224-1236,1284`);
  - the command palette action "Project settings" (`components/CommandPalette.tsx:1803-1828`);
  - the chat header (`components/ChatView.tsx:1174`).
  - The redirect itself is in `routes/projects.$projectKey.tsx:11-15`.

### Modal frame (look)
- **Size:** fills the window below the `sm` breakpoint. From `sm` up it is capped at 80rem × 60rem (1280 × 960 px) with `rounded-2xl` (16 px), a border and `shadow-2xl/40` (`components/AppRouteModal.tsx:86`).
- **Inset from the window edge:** 24 px from `sm` and 40 px from `lg`. The top inset clears native window controls (`AppRouteModal.tsx:61-70`).
- **Animation:** opens and closes with a 150 ms fade plus a scale from 0.985 (`AppRouteModal.tsx:86`).
- **Focus on open** goes to the modal surface, not to its first control, so no stray focus ring shows and phones don't raise the keyboard. On close, focus is not returned to the opener (`AppRouteModal.tsx:23-25,81-82`).
- **Closing with a field focused** blurs it first, so commit-on-blur fields still save (`AppRouteModal.tsx:48-51`).
- **Text selection that ends on the backdrop** does not close the modal (`AppRouteModal.tsx:21-22,71-77`).
- **Where close returns to:** the page the modal opened over. It steps back in history when it can, otherwise navigates with `replace` (`components/sidebar/mainAppLocation.ts:59-77`).

### Content header
- **Height and padding:** 48 px (`h-12`), bottom border, `ps-5 pe-2` (`routes/_chat.settings.tsx:140`).
- **Breadcrumb:** "Settings / General". The parent is muted, the current item is foreground, and the separator is a literal `/` in `text-icon-muted`. Text is 14 px medium with a gap of 8–12 px (`components/settings/SettingsBreadcrumb.tsx:23-40`, `components/WorkspaceBreadcrumb.tsx:13-72`).
- **Right side:** a 28 px ghost icon button with an `X` and the tooltip "Close (Esc)" (`components/AppRouteModal.tsx:102-121`).
- **General only:** a ghost "Restore device defaults" button. It is disabled when nothing differs from the defaults. It opens a destructive confirm that lists every changed setting by name ("This will reset: Theme, Chat width, …") (`routes/_chat.settings.tsx:31-46,143-147`, `components/settings/SettingsPanels.tsx:468-650`).

### Section list (left)
- **Width:** 240 px (`w-60`) on `bg-sidebar` with an end border. Below `md` it shrinks to a 56 px icon rail, and searching expands it over the page (`SettingsSidebarNav.tsx:142-143,258-266`).
- **Groups and their order** (`components/settings/settingsSearch.ts:131-177`):
  - an unlabelled group: General, Appearance, Chat, Diffs & pull requests, Keybindings, Voice, SnapShots;
  - **Agents:** Providers, Threads, Browser, Devices, Scheduled tasks;
  - **Code:** Projects, Source control & issues, Worktrees;
  - **Connections:** Environments, Sharing & access, Secrets (behind a feature flag);
  - another unlabelled group: Maintenance, Labs, About, Archive.
  - Groups whose heading is hidden are set apart by space alone. Visible headings are 12 px medium, muted at 80% (`SettingsSidebarNav.tsx:381-395`).
- **Item:** a 32 px row with an 8 px radius (`--control-radius`), a 10 px inset and an 8 px icon gap. Labels are 14 px, muted at 80%. Hover uses `sidebar-row-hover`; the active item gets `sidebar-row-selected` (white in light mode) and medium weight (`components/ui/sidebar.tsx:755-776`, `index.css:103-111,1178`).
- **Icons:** 16 px Lucide icons, one per section: `Settings2`, `Palette`, `MessageSquareText`, `FileDiff`, `Keyboard`, `Mic`, `Bot`, `MessagesSquare`, `Globe`, `Smartphone`, `CalendarClock`, `PanelsTopLeft` (Projects), `GitBranch`, `FolderTree`, `Server`, `ShieldCheck`, `KeyRound`, `Wrench`, `FlaskConical`, `Info`, `Archive`. SnapShots uses a custom icon (`SettingsSidebarNav.tsx:66-115,128-131`).
- **Detail pages** without their own list entry highlight their parent: Diagnostics highlights Maintenance, and Open source licenses highlights About (`SettingsSidebarNav.tsx:117-121`).
- **Footer:** the T3 Connect sign-in and avatar (`SettingsSidebarNav.tsx:433-440`).

### Search within settings
- **The search box sits at the top of the section list.** It is 32 px tall with a search icon and a `/` keycap hint (`SettingsSidebarNav.tsx:268-322`).
- **Shortcuts:** `/` (outside text fields) or `mod+f` focuses and selects it. Both are ignored inside other dialogs, popups and key-capture fields (`SettingsSidebarNav.tsx:155-193`).
- **Results replace the section list.** Each result is a two-line row: the setting title, then its section name in 11 px muted text, with the section icon. The list is a proper `listbox`/`combobox` pair driven by `aria-activedescendant` (`SettingsSidebarNav.tsx:331-366`).
- **Keys in the search box:** ↑/↓ wrap, Enter opens the result, and Esc clears the query before it would close the modal. With no matches it shows "No settings found" (`SettingsSidebarNav.tsx:231-255,323-330`).
- **The index is hand-written:** one entry per setting with an id, title, page, owning scope and extra keywords. The rank order is exact title, then title prefix, then title substring, then all words in the title, then a phrase anywhere. Entries flagged "secondary" sink. Entries for features this platform lacks are filtered out (`settingsSearch.ts:242+,1604-1686`).
- **Opening a result:**
  - The app navigates to `page#settingId`. The row scrolls to the center (smoothly unless the user prefers reduced motion) and takes focus.
  - The row pulses twice: a 2 px inset ring in 70% primary, 650 ms each time (`components/settings/settingsLayout.tsx:80-155`, `index.css:924-943`).
  - A collapsed "Advanced" fold opens first if the target is inside it (`components/settings/AdvancedSettings.tsx:8-25`).
- **The command palette indexes the same entries,** so a setting can be reached without opening Settings first (`components/CommandPalette.tsx:1831-1836`).

## 2. Sections
One line each. Row titles come from the search index (`settingsSearch.ts:242+`); defaults are in [files-settings.md](files-settings.md).
- **General:** Notifications, Sidebar (project grouping, project order, Working section), Confirmations (unpin, archive, delete, terminal close, quit shortcut), Display (language, time format) (`SettingsPanels.tsx:826-1210`).
- **Appearance:** a theme grid (color scheme, themes, contrast, glass opacity), then Interface (chat width, effort display, math, panel animations, fonts, word wrap) (`components/settings/AppearanceSettingsPanel.tsx:152-496`).
- **Chat:** Composer (send shortcut, rich text, skills in the slash menu), Follow-ups, Activity (tool calls), Screen reader (`SettingsPanels.tsx:1213-1400`).
- **Diffs & pull requests:** diff colors, whitespace, layout, default file state; pull requests open proactively or in T3 (`SettingsPanels.tsx:1623-1743`).
- **Keybindings:** every command with its binding, editable.
- **Voice:** engine, microphone, dictation shortcut, language, dictionary, transcript cleanup.
- **SnapShots:** capture shortcut, sound, flash, animations.
- **Providers:** provider instances, usage sources, text generators, health checks. One environment at a time (`docs/user/project-settings.md:28-29`).
- **Threads:**
  - New projects;
  - New threads (model, fallback model, permissions, workspace), from `SettingsPanels.tsx:1981`;
  - Snooze;
  - Usage limits;
  - Auto-settle;
  - Text generation (`SettingsPanels.tsx:1858-2243`).
- **Browser:** agent browser access, profiles, viewport, zoom, recordings, link handling.
- **Devices:** device sources and simulators.
- **Scheduled tasks:** a list for each environment.
- **Projects:** a per-project page; see section 6.
- **Source control & issues:** automatic pull, agent credits, merge method, issue trackers, writer model, review guides.
- **Worktrees:**
  - Creation: start from remote, plus Submodules under Advanced;
  - Branches;
  - cleanup and location (`components/settings/WorktreesSettings.tsx:67-79`).
- **Environments / Sharing & access / Secrets:** connections, network access and Tailscale, approval of new devices, secret providers.
- **Maintenance:** Updates, Background activity, Diagnostics (`SettingsPanels.tsx:2302-2424`).
- **Labs:** experimental toggles, such as the boring sidebar and two-line folder rows.
- **About:** version, update track, the `t3` command, legal.
- **Archive:** archived threads.

## 3. Page and row anatomy (look)
- **Page width and padding:** the page is centered with a maximum width of 896 px (`max-w-4xl`, "readable"). Wider variants are 1024 and 1152 px. Padding is 20 px (24 px from `sm`) at the sides, 24 px top and 48 px bottom (`components/WorkspacePageContainer.tsx:5-25`).
- **Gaps:** 32 px between sections; the project page uses 24 px (`settingsLayout.tsx:657`, `ProjectSettingsPanel.tsx:538`).
- **The scroll area** fades under the header and reserves gutters on both sides (`settingsLayout.tsx:653-656`).
- **Section heading:**
  - An `h2` at 16 px semibold in the foreground color, with an optional icon. It is indented 12 px (16 px from `sm`) so it lines up with the row text inside the card, and is at least 28 px tall.
  - An optional action can sit at the right end of the heading line. The gap to the card is 10 px (`space-y-2.5`).
  - A section can hide its title visually while keeping it for screen readers (`settingsLayout.tsx:213-265`).
- **Card ("grouped" variant):**
  - `rounded-xl`, which is 12 px because `--radius` is 0.5rem plus 4 px (`index.css:290-294,1116`).
  - A 1 px border at `border/60`, a `card/40` fill and a very faint `shadow-xs/5`.
  - Rows are divided by 1 px top borders at `border/50`.
  - The "plain" variant drops the card and stacks with 4 px gaps (`components/settings/SettingsGroup.tsx:5-29`).
- **Row:**
  - Padding is 12 px horizontal (16 px from `sm`) and 12 px vertical (`py-3`).
  - Each row is a CSS container. At 512 px wide or more it is a two-column grid (`1fr auto`) with a 32 px gap and the control vertically centered on the right. Narrower, the control drops below the text with a 12 px gap (`settingsLayout.tsx:532-573`).
  - **Title line:** 14 px medium, at least 20 px tall. It is followed by the 20 × 20 px inheritance slot and the 20 × 20 px reset slot, 6 px apart (`:540-558`).
  - **Description:** 12 px with normal line height in muted color, at most 672 px wide (`max-w-2xl`), using `text-pretty`. A status line can follow in 12 px muted, 2 px further down (`:559-566`).
  - **Controls:** select triggers in a row are at least 160 px wide (`:569`).
  - **Rows with children** (lists or sub-controls under the main line) use 12 px top and 4 px bottom padding (`:534`).
- **Disabled controls are never just greyed out.** When a control can't be used it is made inert at 50% opacity and wrapped in a focusable span whose tooltip says why (`settingsLayout.tsx:426-478`):
  - "Managed by {org}."
  - "This connection lacks permission…"
  - "Environment-wide setting. Select an environment to change it."
  - "Saved on this device. Select All projects to change it."
- **Unavailable group:** an amber info icon with a 12 px explanation above rows dimmed to 64% (`settingsLayout.tsx:267-285`).
- **Info banner** (`Alert variant="info"`): `rounded-xl` with a 1 px `info/32` border, an `info/4` fill, 14 × 12 px padding, 14 px text, an `Info` icon in `--info` (blue-500 in light mode) and the description in muted text. The project page opens with one: "Can't find a setting? Keep this project picked above and hop to any other settings page." (`components/ui/alert.tsx:7-24`, `ProjectSettingsPanel.tsx:539-547`).
- **"Advanced" fold:** sits at the bottom of a card. The trigger is a 40 px row with 12 px medium muted text and a 14 px chevron that rotates 90° in 150 ms. The opened rows keep the card's dividers (`components/settings/AdvancedSettings.tsx:27-41`).
- **Collapsible sections:** `FoldedSettingsSection` puts the title, a muted summary and an optional control on a 44 px header inside the card (`components/settings/FoldedSettingsSection.tsx:69-101`).
- **Theme tokens:**
  - `--radius` 0.5rem and `--control-radius` 0.5rem (`index.css:103,1116`).
  - Light theme: background zinc-25, card white, border zinc-200, input zinc-300, muted text zinc-500, primary `oklch(0.488 0.217 264)` (a blue-violet).
  - Dark theme: background `#000`; border and input are white at 8% and 11% (`index.css:1116-1176,1193-1223`).
- **Fonts:**
  - Sans: `-apple-system, BlinkMacSystemFont, "Inter Variable", "Segoe UI", system-ui`. Mono: `ui-monospace, "SF Mono", Menlo, Consolas…`. Both can be overridden in Appearance (`index.css:168-176`).
  - Extra small sizes: `text-2xs` is 11 px and `text-3xs` is 10 px (`index.css:179-187`).
  - Icons throughout are Lucide.

## 4. Controls and saving

### Controls
- **Switch:**
  - The thumb is 16 px on desktop (20 px on touch), so the track is about 30 × 18 px. It is fully rounded with 2 px padding (`components/ui/switch.tsx:5-37`).
  - On is `bg-primary`; off is `bg-input`.
  - A "mixed" state centres the thumb at 70% opacity and sets `aria-checked="mixed"`.
- **Select (`size="sm"`):**
  - 28 px tall on desktop, `rounded-lg`, with a 1 px `border-input` on `bg-background` and `shadow-xs/5`.
  - The popup aligns to the trigger's end.
  - Rich options show an icon and label plus a 12 px muted description. The permissions select is an example (`components/ui/select.tsx:13-32`, `ProjectDefaultsSettings.tsx:391-437`).
- **Text input (`size="sm"`):** 26 px tall on desktop and 256 px wide (`sm:w-64`) for the project name (`components/ui/input.tsx:22-25`, `ProjectSettingsPanel.tsx:576-595`).
- **Buttons:** row actions use `size="sm"` (28 px on desktop) with the `outline` variant, for example "Choose icon" and "Archive". Destructive actions use `destructive-outline` with a `Trash2` icon. Buttons scale to 0.97 when pressed (`components/ui/button.tsx:9-49`).
- **Model chip plus effort chip** (the Model and Fallback model rows):
  - The composer's own `ProviderModelPicker` and `TraitsPicker` are reused, restyled as bordered selects: `border-input`, `bg-background`, `shadow-xs/5` and a softened chevron (`settingsLayout.tsx:188-190`, `ProjectDefaultsSettings.tsx:170-213`).
  - Each chip is 28 px tall with an 8 px radius (`components/chat/ComposerControl.tsx:10-18`).
  - The model chip shows a 16 px provider icon, then the model name (`components/chat/ProviderModelPicker.tsx:417-436`). The effort chip shows a `Gauge` icon and the effort label (`components/chat/TraitsPicker.tsx:567-580`).
  - The effort chip is hidden when values are mixed or the model is unavailable.
  - The model popover is the same three-column picker as in the composer ([model-picker.md](model-picker.md)).
- **Number and text fields save on blur or Enter.** Escape cancels the draft without closing Settings (`SettingsPanels.tsx:778-824`).
- **Long text (thread title instructions)** is a 4-row textarea that saves on blur only if it was edited (`components/settings/ThreadTitleInstructionsSettings.tsx:39-60`).

### Saving
- **Writes are optimistic.** The patch is added to a pending-writes atom, so the control shows the new value right away. The entry is dropped once the server pushes the saved settings back, or when the save fails (`components/settings/scopedSettings.ts:64-125`, `useScopedSettings.ts:78-114`).
- **Validation happens before writing.** Picking a model the provider can't serve shows the toast "Default model not saved" with the reason and writes nothing (`ProjectDefaultsSettings.tsx:126-137`). An empty project name gives a warning toast, "Project title cannot be empty" (`ProjectSettingsPanel.tsx:319-327`).
- **Errors are toasts.** "Setting not saved", or "Setting saved on some environments" listing which saved and which failed (`useScopedSettings.ts:116-140`).
- **No undo.** Recovery is a per-row reset or "Restore device defaults".
- **Per-row reset:**
  - A 20 px ghost `Undo2` icon button (12 px glyph) with the tooltip "Reset to default". It appears only when the value differs from the default (`settingsLayout.tsx:585-618`, `ProjectDefaultsSettings.tsx:162-169`).
  - In project scope the same slot becomes "Reset to inherited value", which clears the override (`settingsLayout.tsx:407-425`).

## 5. Inheritance and overrides
- **Order of precedence:** project override, then environment value, then `t3.json` (for file-backed keys such as workspace mode and submodules), then the built-in default (`docs/user/project-settings.md:186-191`, `packages/shared/src/projectSettings.ts:55-108`).
- **Where overrides live:** in the environment's server settings, under `projectSettingsOverrides[projectId]` (`projectSettings.ts:110-127`).
- **Which page edits what:** the same Threads, Source control and Worktrees rows edit environment defaults under "All projects" and project overrides once a project is picked (`docs/user/project-settings.md:15-19,175-181`).
- **Device-only pages** (General, Appearance, Chat, Diffs, SnapShots, Environments, Access, About) hide the scope sentence (`components/settings/SettingsScopeSentence.tsx:51-61,75`).
- **The scope sentence:**
  - "Applying settings for [Project ▾] across/on [Environment ▾] in [Checkout ▾]", in 14 px muted text with outline `sm` buttons (`SettingsScopeSentence.tsx:89-146,196-210`).
  - Each button shows an icon (project favicon, machine icon or folder), the label and a chevron.
  - With more than 6 options a button opens a searchable combobox; otherwise a radio menu. "All …" comes first, then a separator (`:195-284`).
  - The selection sits in the URL query (`project`, `machine`, `checkout`) and carries over as you move between sections (`components/settings/settingsScopeNavigation.ts:17-27`).
- **The layers marker:**
  - Lucide `Layers` at 12 px inside a 20 px ghost button, placed right after the row title (`components/settings/SettingInheritance.tsx:282-307`).
  - Its color shows the state: primary (blue) when "Overridden for this project", amber when "Mixed across selected environments", muted otherwise.
  - Its tooltip summarises the source: "Inherited from {env}", "Inherited from the repository's t3.json", "Set on the environment" or "Built-in default". It adds " · N project overrides" when projects override the value (`settingsLayout.tsx:490-506`, `SettingInheritance.tsx:222-225`).
  - The marker stays hidden until hover or focus when the value is the default or set right here on the environment, because then it adds nothing (`settingsLayout.tsx:542-553`).
- **The layers popover** is 320 px wide (`md`). For each environment it shows the chain, top to bottom (`SettingInheritance.tsx:121-176,309-353`):
  - Project;
  - the environment;
  - `t3.json`;
  - Default.
  - Each line pairs a label with a value; layers that aren't set read "Inherits".
  - The winning layer has a 6% foreground tint, medium weight and a primary check mark.
  - Environments that resolve the same way are merged ("Same on all machines").
- **"Overridden by" list:** when you edit an environment value that projects override, the popover lists each overriding project with its value. Clicking a project switches the scope to it; "Reset it" / "Reset all" clears those overrides (`SettingInheritance.tsx:423-505`, `docs/user/project-settings.md:24-27`).
- **Clearing one project override:** use the row's `Undo2` reset ("Reset to inherited value"). It is visible only while that project's value comes from its own override or is mixed (`settingsLayout.tsx:411-422`).
- **Mixed values:**
  - When the selected targets disagree, the control shows "Mixed" (selects and the model chip) or a centred thumb (switches).
  - Picking a value writes it to every selected target (`docs/user/project-settings.md:21-22`, `ProjectDefaultsSettings.tsx:181-187,245-261`).
  - The Model row also explains in its status line: "Defaults differ. Select one environment or choose a shared model to edit effort." (`ProjectDefaultsSettings.tsx:153-161`).
- **Descriptions change with scope.** For example, "Model for new threads in this project." versus "Default model for new threads. Projects can override it." (`ProjectDefaultsSettings.tsx:146-152,231-235`).

## 6. Projects page
- **There is no separate list page.** With no project picked, the page shows the scope sentence, the text "Choose a project to manage its name, icon, checkouts and actions.", and a card listing every project (`components/settings/ProjectsSettings.tsx:48-54`, `components/settings/SettingsScopeNotice.tsx:88-103,127-186`). Each project row has:
  - a 16 px favicon and the name in 14 px medium;
  - a 12 px muted line, "Archived · /path" (truncated, with a tooltip);
  - a chevron at the end;
  - padding of 12 px (16 px from `sm`), with the first and last rows rounded to fit the card (`components/settings/itemRows.ts:1`).
- **The per-project page, top to bottom** (`components/settings/ProjectSettingsPanel.tsx:536-733`):
  - **Info banner** (see section 3).
  - **Overview card** (titled "Project" but the title is hidden):
    - **Name:** a 256 px text input that saves on blur or Enter. A reset appears only for scratch projects (`:556-597`).
    - **Project icon:** the description reads "lucide-name · color", the emoji, the favicon path or "Automatic". The control is a 24 px preview plus "Choose icon" (icon, emoji or monogram picker) and "Choose file" (image from the repository), both outline buttons, with a reset back to automatic (`:598-643`; `docs/user/project-settings.md:276-286`).
    - **Pin:** a switch, "Pinned projects come first in project pickers." (`:646-662`).
    - **Archive:** an outline button, "Archive" or "Unarchive", with the description "Hides the project from project pickers. Threads and files are kept." It has no confirm and no undo; a failure shows a toast (`:663-689`).
  - **New threads:** Model (model chip plus effort chip) and Workspace (Local / Worktree select), each with a layers marker. Fallback model and Permissions are not on this page; they only show on Threads (`components/settings/ProjectDefaultsSettings.tsx:272,353-363`).
  - **Actions:** the project's script list, with "Add action", "Import scripts" from `t3.json`, and a warning row if `t3.json` is invalid (`components/settings/ProjectActionsSettings.tsx:118-222`).
  - **Checkouts:**
    - An intro line explaining that the grouping rule can move a checkout to another project.
    - One row per environment with its path. The control is a grouping select ("Default (Repository)" or a specific rule), plus "Remove" when the project has more than one checkout (`ProjectSettingsPanel.tsx:433-534`).
  - **Danger:** "Remove project" (or "Remove checkout" / "Remove all entries"), a `destructive-outline` button with a trash icon (`:696-733`).
- **Remove confirmation:** a native destructive confirm that reads (`packages/client-runtime/src/state/projectRemoval.ts:38-86`):
  - `Remove project "X" and delete its N threads?`
  - Path: …
  - Environment: …
  - "Those threads and any archived threads are deleted from T3 Code. Provider sessions are kept."
  - "This removes only the project entries, not the files on disk."
  - "This action cannot be undone."
  - After removal, drafts move to a remaining checkout or are cleared, and the page returns to the project list (`ProjectSettingsPanel.tsx:378-430`).
- **Archived projects** stay pickable in Settings, marked "Archived", so they can be unarchived there (`docs/user/project-settings.md:58-61`, `SettingsScopeSentence.tsx:439-441`).
- **Permissions:** without write access, the page shows a one-line explanation at the top of the card and disables the controls (`ProjectSettingsPanel.tsx:549-555`).

## 7. Keyboard and accessibility
- **Opening:** `CmdOrCtrl+,` on desktop, or the palette's "Open settings". The web app has no default shortcut for it: `packages/shared/src/keybindings.ts` has no settings command.
- **Closing:** Esc, unless a search query, a dirty number field or a popup consumes it first (`SettingsSidebarNav.tsx:233-238`, `SettingsPanels.tsx:813-819`).
- **Moving between panes:** `F6` / `Shift+F6` (`mod+F6` in browsers) moves between the section list and the open section. Each is a labelled `region` workspace area ("Settings sections", "Settings content") (`docs/user/keyboard-focus.md:13-25`, `SettingsSidebarNav.tsx:136`, `routes/_chat.settings.tsx:126`, `lib/workspaceFocus.ts:12-19`).
- **Section list:** Tab moves through items. ↑/↓/Home/End move focus without opening a section, and Enter opens it (`components/settings/settingsSectionNavigation.ts:6-33`, `docs/user/keyboard-focus.md:36-37`). Group headings are outside their `role="group"` and linked by `aria-labelledby`, so NVDA does not read them twice (`SettingsSidebarNav.tsx:396-397`).
- **Search:** `/` or `mod+f` (section 1).
- **Rows:** a row that is a search target is focusable (`tabIndex=-1`), so a search jump lands focus on it. Inert controls stay focusable through their tooltip wrapper, so keyboard users can read why (`settingsLayout.tsx:426-445,530`).
- **Labels:** every control has an explicit `aria-label`, for example "Grouping rule for {path} on {environment}" and "Reset {title} to default" (`ProjectSettingsPanel.tsx:121-126`, `settingsLayout.tsx:414-418`).
- **Reduced motion** turns off the smooth scroll and the pulse; the focus outline marks the target instead (`settingsLayout.tsx:82,149`, `index.css:940-943`).

## Implications for Compi
Compi's settings (`crates/compi-client/src/gui/workspace/settings.rs:1-17`) is already close in shape. It opens as a content pane beside the focused pane. Its header has search and section icons, and each section page has a heading line and cards of rows built from a single `ROWS` table. The sections are Appearance, Interface, Notifications, Terminal, Chat, Keyboard, Monitor, Updates and Advanced (`crates/compi-protocol/src/lib.rs:1771-1788`). Scope is a global/window switch in the page header. There is no Projects section and no per-project layer.

What to borrow:
- **Projects under the same row system.** Add a Projects section. Mirror the overview card (Name, Icon, Pin, Archive), a "New threads" card (model chip + effort chip, workspace), and Danger last. Reuse the "scope sentence" idea in place of a separate project screen: Compi's existing scope row becomes "Applying settings for [All projects ▾]".
- **One inheritance marker per row.** Add a 20 px layers icon after the title. Make it primary when overridden, amber when mixed, and hidden on hover-less defaults. Its popover shows the chain Project → Global → (window) → Default with the winner checked. Compi's window/global scope fits the same chain [INFERENCE].
- **Reset in a fixed 20 px slot after the title.** It should mean "back to inherited value" in project scope and "back to default" otherwise. Skip a global undo.
- **Instant saves with explicit commit rules.** Switches and selects save on change; text saves on blur or Enter; Esc in a dirty field cancels instead of closing; failures show a toast. Make daemon writes optimistic, as T3 does with pending writes.
- **Visual constants to match:**
  - Card radius 12 px with a 1 px border at 60% and faint fill.
  - Row padding 12/16 px, title 14 px medium, description 12 px muted (672 px max).
  - Two-column layout at ≥ 512 px row width. Compi already stacks below 440 px; consider 512.
  - Section gap 32 px, page max 896 px. Compi's 1280 px is T3's modal width, not its page width.
- **Search jumps should land on the row,** scroll it to the center, focus it and pulse it twice, opening any Advanced fold first. Compi's derived keyboard order makes this cheap.
- **Disabled controls explain themselves** through a focusable tooltip (for example, project-scoped versus global-only rows) instead of simply greying out.
- **Keyboard:** `mod+,` to open, `/` to search, ↑/↓ in the section list without opening, and `F6` between the list and the page.
