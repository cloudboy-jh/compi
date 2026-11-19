# T3 Code research

How [T3 Code](https://github.com/pingdotgg/t3code) behaves, read from its source and docs at commit `8cf97b5f` (2026-11-18), as reference for Compi's chat-first mode, threads and sidebar. Borrow behaviour, not layout or look. Lines marked [INFERENCE] were not seen in source.

Cited paths are inside the T3 Code repository. Unless a report says otherwise they start at the repository root; `composer.md` cites from `apps/web/src`, and `timeline.md` from `apps/web/src/components/chat` (except paths starting with `docs/`).

- [Composer](composer.md): the input box, attachments, menus, send states, queue, recall, stash.
- [Model picker](model-picker.md): the picker, mid-thread model and agent switches, handoff rows, context window.
- [Timeline](timeline.md): how agent work, tool calls, sub-agents, approvals and questions show in a thread.
- [Sidebar](sidebar.md): projects, threads, statuses, drag and keyboard, terminals per thread.
- [New thread and sidebar tools](start-and-sidebar-tools.md): the draft screen that becomes a thread, sidebar search, the thread row.
- [Threads](threads.md): thread data, agent sessions over time, persistence, forks, restart recovery, handoff selection.
- [Agent fidelity](agent-fidelity.md): what each agent's protocol shows (Claude, Codex, OpenCode, Pi, ACP) and what plain ACP loses.
- [Files and settings](files-settings.md): file picker, file tree, chat, thread and project settings with defaults.
- [Settings screens](settings-ui.md): the settings modal, section list and search, row and card look, controls and saving, project overrides, the Projects page.
- [Project icons](project-icons.md): automatic favicon lookup, monogram fallback, icon and file pickers, caching, sizes and colours.
