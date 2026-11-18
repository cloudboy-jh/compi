# T3 Code composer interactions

## Typing and rich text
- **Markdown as you type** (`docs/user/composer.md:18-46`): text is stored as Markdown and shown styled. When the caret is on styled text, the raw markers (`**`) show and can be edited; styling comes back when the caret leaves. Lines starting `- `, `1. `, `- [ ] `, `> `, `# ` or `---` turn into lists, task lists, quotes, headings and rules. ` ``` ` then Enter opens a code block. The agent gets exactly the text typed.
- **Chips act like one character**: they are atomic, selectable editor nodes (`components/ComposerPromptEditorTiptap.tsx:270,337,376,441,577`). Left/Right moves past a whole chip in one step (`:1229-1242`). Shift+Tab right after a citation chip moves focus to that chip's comment button (`:1245-1266`).
- **IME is respected**: Enter during composition (`isComposing` or keyCode 229) is swallowed (`:1267`), and all key logic ignores composing events.
- **Limits**: a message holds up to 120,000 characters, and a longer draft stays in the composer. A paste of 32 KiB or more becomes a `.txt` attachment, with a toast that says how to keep it inline (`Cmd/Ctrl+Shift+V`). A smaller paste also becomes an attachment if inserting it would go over the limit (`components/chat/useComposerAttachmentIntake.ts:413-494`).

## Enter / Shift+Enter
The decision is made in `composer-logic.ts:67-104` and `components/chat/composerCommandKey.ts:214-300`.

| Situation | What the key does |
|---|---|
| Composing, key repeat, or Enter while the mobile soft keyboard is open | Nothing (no send) |
| Plain Enter | Sends, unless the send-shortcut setting requires Mod (`mod-enter`, or `mod-enter-multiline` when the prompt has a newline) (`:57-65`) |
| Shift+Enter or Alt+Enter | Not a send. Continues a list or quote (`listContinuationForEnter`). On an empty item it moves out one level, then leaves the list |
| Tab / Shift+Tab | Indent or outdent a list item. In a code block, indent or outdent the selected lines (`ComposerPromptEditorTiptap.tsx:1276`) |
| Enter inside a task item | Falls through to the editor |

- **Order of priority** (`composerCommandKey.ts:228-296`): Escape → open menu → Tab accepts the next-prompt suggestion → Up/Down prompt history → send → list edits.
- **Enter with an empty draft and Resume available** does the same as clicking Resume (`components/chat/ChatComposer.tsx:2206-2212`).

## Trigger menus: `/`, `$`, `@`, `#`
- **Opening** (`composer-logic.ts:262-297`): the menu looks at the whitespace-delimited token before the caret.
  - `/` opens commands. Things that look like a path (`a/b`) don't open it.
  - Any currency symbol (`\p{Sc}`, usually `$`) opens skills. A currency amount like `$5` does not.
  - `#` opens pull requests.
  - `@` opens one list with threads, machines, models and workspace files (`components/chat/useComposerMenuItems.ts:240-283`).
- **Filtering**: the text after the trigger is the query. PR text and number searches wait 180 ms before running (`useComposerMenuItems.ts:122-176`). While results load the menu says "Searching…"; with no results it shows an empty message (`components/chat/ComposerCommandMenu.tsx:172-186`).
- **Keys**:
  - ↑/↓ move the highlight and wrap around (`ChatComposer.tsx:2145-2157`).
  - Enter or Tab picks the highlighted item, or the first item if none is highlighted (`composerCommandKey.ts:245-258`).
  - Escape closes the menu and remembers the dismissal by trigger kind + start position, so the menu stays closed while you keep typing that token. It reopens only when a different token starts (`components/chat/useComposerTriggerState.ts`).
  - When the query changes, the highlight goes back to the first item (`components/chat/composerMenuHighlight.ts`).
- **Mouse**: hovering moves the highlight; `mousedown` is cancelled so the editor keeps focus; the active row scrolls into view (`ComposerCommandMenu.tsx:127-131,227-236`).
- **Picking an item** (`ChatComposer.tsx:1951-2136`): the trigger range is replaced with the item plus a trailing space, and an existing space after it is absorbed (`extendReplacementRangeForTrailingSpace`).
  - The replacement only happens if the text still matches what the menu was built from (`expectedText`).
  - A one-frame lock prevents double-picks.
  - Special commands: `/model` deletes the token and opens the model picker; `/clear` deletes it and arms a fresh session; `/side` and `/btw` stay as text.

## Inline chips (context)
- **Where they go**: attached context lands at the caret as a chip — terminal excerpt, review comment, PR, thread, file, or quote (`docs/user/composer.md:349-389`). You delete a chip like one character and can move it with cut and paste.
- **Files exist only as chips**: deleting a file's last chip removes the file from the message. Undo puts it back, because deleted payloads are kept until the draft's undo history ends (`components/chat/composerContextUndo.ts:41-114`).
- **Images** have both a shelf thumbnail and a chip:
  - Deleting the chip leaves the thumbnail.
  - Removing a thumbnail whose image is still mentioned in the text asks for confirmation first, then removes it and every reference (`useComposerAttachmentIntake.ts:374-411`).
  - Images get a chip only when the prompt already has text, there's a selection, or it's a question answer (`:141-146`).
- **Quotes**: a quote chip shows its comment, or a preview of the quote. A pencil button edits the comment. Cancelling a new citation removes it along with its surrounding space (`ComposerPromptEditorTiptap.tsx:490-503`).
- **Copy and paste between drafts** brings the chips and what they point to. While a chip's content is being fetched again it has a dashed outline. Sending at that point is blocked with a "Still bringing attached context…" toast (`ChatComposer.tsx:2198-2205`).

## Drag and drop / paste
- **Drop targets**:
  - The whole conversation area accepts OS files and folders and shows `WorkspaceFileDropOverlay` (`components/ChatView.tsx:3356-3450`).
  - The composer form itself takes internal file-tree mentions, PRs and sidebar threads. It highlights with an accent ring (`ChatComposer.tsx:3458-3475,3526`).
  - A window-level `dragend` clears the highlight when Escape cancels a drag (`:3019-3028`).
- **Paste with nothing focused**: the composer blurs when you click the timeline. A text paste after that still goes to the end of the composer (`components/chat/usePasteToFocusComposer.ts`). Typing with nothing focused also goes to the composer, unless an input, button or open popup owns the key (`components/chat/composerFocusRedirect.ts`).
- **Intake pipeline** (`useComposerAttachmentIntake.ts:124-372`):
  - Checks run synchronously and reserve slots, so pastes that happen at the same time still respect the 100-file cap.
  - Images show a "Preparing" placeholder while being compressed or converted from HEIC. The placeholder keeps the same id when it becomes the real thumbnail, so the tile never flickers.
  - Duplicate images are prepared without showing a tile.
  - Errors appear in the thread error area: unsupported type, empty file, over 10 MiB after compression, server has no file support, too many files.
- **Upload states** (`components/chat/ComposerAttachmentStrip.tsx:290-436,501-545`): preparing → uploading (progress) → failed (Retry + Remove) → "Attach again" / "Remove to send" for expired stash files.
- **Sending is blocked** while an image is still compressing. The toast says "Send again once its thumbnail appears" (`ChatComposer.tsx:2187-2196`).

## Send button states
In `components/chat/ComposerPrimaryActions.tsx`, the button is chosen in this order:

1. **Question pending**: Previous / Next / Submit buttons, plus Stop (`:307-370`).
2. **Running with an empty draft**: only **Stop**.
   - Clicking moves it to "Stopping…" and disables it.
   - It re-enables after 10 s.
   - Once the stop lands, the button that replaces Stop ignores clicks for 500 ms, so repeated Stop clicks can't trigger Resume (`:102-139`).
   - If you type while a stop is pending, Stop stays at the right edge next to the send button (`:296-300`).
3. **Resume ▶**: empty draft with a held queue.
4. **Update queued message ✓**: while editing a queued message.
5. **Compact and send**: an optional toggle beside the send button.
6. **Running with content**: shows **Queue** or **Steer**, depending on the follow-up setting.
   - Holding Cmd/Ctrl switches the icon live to the opposite action, and Mod-click uses it (`:265-276,432-440`).
   - The opposite-action shortcut is Mod+Enter; when Mod+Enter is the normal send, it's Shift+Mod+Enter (`docs/user/composer.md:96-108`).
7. **Idle**: ↑ Send.

- **Busy and disabled**: a spinner shows while busy. The tooltip explains why the button is disabled ("Environment disconnected", "Preparing worktree"). On mobile the buttons use `preventDefault` on pointerdown so the editor keeps focus.

## Queued message editing
- **Starting an edit**: the pencil or `Alt/Opt+↑` with the caret at the start (`ChatView.tsx:2814-2817`). The message loads into a separate draft, so your current draft is untouched and comes back afterwards (`components/chat/useQueuedRunEdit.ts:236-300`).
- **During the edit**: the original stays in the queue, highlighted. Its existing attachments have remove buttons, and removals only take effect on save.
- **Save (✓)**: uploads new files first. If the draft changed during the save, it asks you to review and save again. Saving with the opposite-action modifier saves and then steers (`:383-467`).
- **Cancel** discards the edit and refocuses the composer.
- **If the run starts or is removed during the edit**: a changed edit moves into the thread draft if that draft is empty; otherwise it's discarded, with a toast either way (`:302-332`).

## Prompt recall (↑/↓)
In `components/chat/composerCommandKey.ts:166-200` and `components/chat/composerPromptHistory.ts`:
- **When ↑ recalls**: the draft is empty, there are no attachments or context, the caret is on the first *visual* line, and no modifier is held.
- **Walking**: ↓ walks forward; going past the newest prompt clears the composer.
- **What's restored**: only text you typed, plus thread and machine chips. Consecutive duplicates are collapsed; automated and agent-sent messages are skipped.
- **Ending recall**: editing the recalled text turns it into a normal draft. Recall is tracked per thread, so it never carries over to another thread.

## Stash (Mod+S)
In `components/chat/useComposerStash.ts`:
- **Mod+S always captures the key**, so the browser's save dialog never opens.
- **Saving**: the entry is saved first, then the composer is cleared, and image encoding happens afterwards. That way typing during encoding isn't lost and a crash doesn't lose the prompt. The badge pulses.
- **Refusals and limits**: stashing is refused while uploads or compression are running. When the stash is full, the oldest entry is dropped with a warning.
- **Restoring**: it is appended below the current text with a blank line in between.
- **The stash menu closes** when you type or when a trigger menu opens (`:740-747`).

## Footer row and collapsed composer
- **Footer order** (`ChatComposer.tsx:3805-3918`), left to right:
  1. Attach button.
  2. Controls, right-aligned: the model picker (which also holds effort, speed and permissions), a permissions chip shown only when the mode isn't your default, and an overflow "…" menu for controls that don't fit (`components/chat/ComposerControlsBar.tsx`, `components/chat/CompactComposerControlsMenu.tsx`).
  3. Dictation (mic, recording pill).
  4. The primary button.
  - The banner dock above the composer holds the stash badge.
- **Collapsed composer** is mobile only: it collapses when unfocused, single-line and with no pending question (`ChatComposer.tsx:1042-1047`). It becomes a one-line preview plus Stop, and tapping it expands and focuses at the end. The height change is a smooth animation (Web Animations API) that can be interrupted and picks up from where it was (`components/chat/useComposerRestingTransition.ts`).

## Focus rules
- **Opening a thread** focuses the composer one frame later, unless a sidebar rename field took focus (`components/chat/useFocusComposerOnThreadOpen.ts`).
- **After a model pick** focus goes back to the composer, but not while the picker is still open (`components/chat/useComposerFocus.ts`).
- **On mobile, sending blurs the editor** to close the keyboard (`ChatComposer.tsx:2159-2171`).
- **Escape with an empty draft** dismisses the suggested next prompt, so Tab moves focus again (`composerCommandKey.ts:235-237`).
