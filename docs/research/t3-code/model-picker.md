# T3 Code: model picker and mid-thread switching

## Summary
- The picker is a popover with three columns. Opening it focuses the search box. Picking a model keeps the popover open and moves focus to that model's effort levels.
- A model change within the same provider is saved to the thread right away and shown inline in the timeline as `old → new`.
- Moving to a different provider creates a recorded **Context handoff**. It shows as a pending row at the bottom of the timeline until the next send. T3 Code never hides this summary inside a prompt without recording it.
- Context problems are handled for you. If the handoff doesn't fit, a fresh provider conversation starts. You never compact by hand, and your request is never shortened.

## 1. Opening
- Ways to open it:
  - Click the model trigger in the composer (provider icon, model name, effort suffix).
  - Press `mod+shift+m`. `mod+shift+e` opens the same popover with focus on effort.
  - Press `mod+shift+a` to go straight to permissions.
  - Evidence: `docs/user/keybindings.md:33-36`, `ProviderModelPicker.tsx:212-220`.
- If the popover is already open, a shortcut moves focus inside it instead of closing it (`ProviderModelPicker.tsx:351-361`).
- Each open asks the server for newly released models in the background (`ProviderModelPicker.tsx:145-148`).
- While it's open, the page behind it can't scroll (`:37-84`).

## 2. Structure (docked composer popover)
- **Provider rail on the left:** Favorites first, then each provider instance. A selection bar slides between rows (`ModelPickerSidebar.tsx`).
  - The rail disappears while you search.
  - Search ranks models across all providers, including provider names, sub-providers and favorites (`ModelPickerContent.tsx:663-673`, `:657`).
  - Without a search, the list shows only the selected tab's models (test comment at `ChatView.handoffCompaction.browser.test.tsx:103`).
- **Model list:**
  - Favorite models are grouped first, and each row has a star toggle.
  - Rows can carry a **New** or **Unavailable** badge.
  - Older models sit behind a collapsible toggle at the end of the list (`ModelListRow.tsx`, `ModelPickerContent.tsx:697-729`).
  - Disabled rows show their reason in a tooltip.
- **Effort column:** an "Effort" or "Options" heading. It lists the model's reasoning levels and marks one as **Default**. Two-state options toggle in place; options with more values open a small side menu (`ComposerModelEffort.tsx:487,524`).
- **Footer:** response speed (Fast/Standard) one click away, and a **Permissions** menu one level deeper (`ComposerModelEffort.tsx:665,708`). The permission choices are a horizontal toggle. Hovering a mode previews its description (`ComposerFooterControls.tsx:101-133`).
- The popover keeps a fixed height, so switching tabs or models never resizes it (`ProviderModelPicker.tsx:452-454`, `ModelPickerContent.tsx:605`).
- Shift-click selects several models for a multi-model send (`:924-929`).

## 3. Keyboard
Source: `keybindings.md:46-50`, `ModelPickerContent.tsx:776-990`.
- **Left** in an empty search box, or **Shift+Tab**, moves to the provider rail.
- **Up/Down** moves through the rail; **Enter** chooses.
- **Right** goes back to search (`ModelPickerSidebar.tsx:97`).
- **Right with the cursor at the end of the search text** chooses the highlighted model first, then moves to its effort levels. **Left** returns to search.
- **Escape** closes the popover.
- `mod+shift+up` / `mod+shift+down` cycle providers. They wrap, skip disabled providers, clear the search and announce the provider name to screen readers.
- Numbered jump shortcuts pick one of the first N enabled models directly and announce `Model: X`.
- Effort can be changed one step without opening the picker. Increase and decrease wrap around and are unbound by default.
- Thread jumps are turned off while the picker is open (`keybindings.md:140`).

## 4. What selecting does mid-thread
- **Same provider:** the pick updates the composer draft and the remembered ("sticky") selection, then saves it to the thread right away (`useApplyThreadModelPick.ts`).
  - Options you last used with that model come back automatically.
  - Re-picking the current model retries a save that failed.
  - If the save fails, a toast says "Could not apply model selection".
  - The next user message shows a divider listing only what changed: `Opus Medium → Opus High`, or just the effort (`V2LifecycleRow.tsx:640-662`, `TimelineUserMessageRow.tsx:274-281`).
- **Other provider:**
  - The pick stays in the draft and isn't saved to the thread until you send. It could replace a live session that has an open question (`useApplyThreadModelPick.ts:31-33`).
  - The timeline footer immediately shows **"Context handoff pending"**: `from model → to model` with provider icons (`ChatView.tsx:3189-3197`, `handoff.ts:26-50`).
  - Instances that share a continuation group don't need a handoff.
  - Once sent, the row becomes **"Context handoff"**, or a danger-toned row if it failed (`V2LifecycleRow.tsx:94-99,664`).
- **Where provider switching isn't supported:**
  - Other providers stay in the rail but are greyed out. Their tooltip says "{provider} is unavailable in this thread. Start a new thread to switch providers."
  - A blocked pick shows a warning toast and returns focus to the composer (`ModelPickerContent.tsx:829-837`, `useComposerSelection.ts:138-160,206-217`).
- **Other timeline markers:**
  - `/clear` shows **New session** and keeps the earlier history in view (`composer.md:296-300`).
  - Forks show "Forked from conversation" with a button to open the source.
  - Compaction shows a separator row: "Context compacted 250k → 40k tokens". It shimmers while running and turns red with the failure text if it fails (`MessagesTimeline.tsx:1736-1800`).

## 5. Context window and compaction
- The thread details card can show a context bar: used / max tokens.
  - It turns to a warning color at 80% and an error color at 90%.
  - A tick marks the auto-compact threshold.
  - The tooltip says "Compacts automatically at about N tokens."
  - It's opt-in under Settings → Appearance, and appears only after the provider reports usage (`ThreadDetailsUsage.tsx:93-143`, `composer.md:302-304`).
- **Returning to a large idle Claude thread** after its prompt cache has expired:
  - A **Compact** chip with the token count appears next to send, and the send button becomes **"Compact and send"**.
  - Clicking the chip switches it to **Keep**, which sends with full history for that one message (`providers-claude.md:61-67`, `ComposerPrimaryActions.tsx:383-394`).
  - The chip doesn't appear when a handoff is pending. Switching to another provider removes it (`contextWindow.ts:284`; tested in `ChatView.handoffCompaction.browser.test.tsx:80-128`).
  - Picking a model leaves the picker open; Escape closes it.
- Changing the model or context-window options throws away old usage numbers and compaction thresholds (`portable-handoffs.md:36-39`).

## 6. Failure and "no room" recovery (`portable-handoffs.md:20-28`)
- If the target conversation can't fit the handoff, T3 Code starts a fresh provider conversation with a limited slice of saved history and sends your request automatically.
- If even a fresh conversation can't fit the request and its attachments, you're told to shorten the request or pick a larger-context model. The request is never trimmed.
- Background work keeps running; let it finish or stop it before retrying.
- The handoff picks messages by budget; it is not a written summary. The agent can read omitted history with a thread-reading tool.
- [INFERENCE] I didn't find a dedicated "no room" component. In the UI this probably shows up as the handoff or new-session rows plus the error-toned handoff row.

## 7. Defaults
- Settings → Threads → **Model** sets the default for new threads.
  - The environment default can be **Automatic** ("selected automatically").
  - Projects can override it.
  - A reset button returns to the default.
  - A **Fallback model** is used when the default provider is out of usage (`ProjectDefaultsSettings.tsx:139-160,278`).
- Permissions follow the same layering. New threads use the configured default, not the thread you were viewing (`permission-modes.md:8-11`).
- Each project can turn a provider on or off; its pickers follow, and a turn already running finishes (`project-settings.md:32-38`).
- [INFERENCE] Picks are also remembered across threads (`setStickyModelSelection`).

## 8. Unavailable or unauthenticated providers
- **Rail:** the provider is dimmed and not clickable. Its tooltip reads "X: Disabled in settings." or "X: Unavailable. {reason}" (`ModelPickerSidebar.tsx:16-45,146-182`).
- **Picker body:** if a provider isn't installed, isn't signed in, or has no usable models, the provider's status message appears with **Open provider setup**. That closes the picker and opens Settings → Providers for that instance. An "update required" notice links to provider settings (`ModelPickerContent.tsx:128-238`).
- **Current model unavailable:** it stays shown as the selection, with an **Unavailable** badge on the trigger. Re-picking it keeps it; providers that can't be chosen are skipped (`ProviderModelPicker.tsx:162-171,425-429`, `ProviderModelPicker.logic.ts`).
- **Picker initial tab:** the active provider when it's locked, needs setup or has an unavailable selection; otherwise Favorites if there's a usable favorite (`ModelPickerContent.tsx:423-449`).

## Design rules for Compi
1. Picking keeps the user in the flow: focus moves to the next decision (effort) instead of closing.
2. A same-provider change is saved instantly and shown as a small change-only divider.
3. A cross-provider change stays a draft until sent; preview it as a pending handoff row.
4. Never block the user: show why something is disabled and give a path to setup.
5. Keep the popup size fixed, and announce keyboard actions to screen readers.
