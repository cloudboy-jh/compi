# 0.4.1

- Hover a tab for its full title and its panes' program status
- Right-click a pasted image to open it in a pane beside its terminal
- File tabs show Edit / Diff only when there is a choice
- macOS: the keyboard no longer gets stuck in a browser page
- Updating ends running shells: the daemon restarts

## Fixes

- Hovering a tab shows its full title and, when programs report status, the state its panes most need you for and each pane's records. Before, only the small status glyph had a tooltip.
- A pasted or dropped image's card has a right-click menu: **Open**, **Open in pane beside** (the image in a Files pane split beside the terminal), **Copy path** and **Dismiss**. A click still opens it over the window.
- The **Edit / Diff** switch on a file tab shows only when it offers a choice: **Diff** for a file with uncommitted changes, **Read** or **View** for Markdown and SVG. Images, PDFs and media read **View** instead of Edit.
- `compi edit FILE --pane` on a Files pane opens the file in that pane, as documented, instead of splitting a new pane or saying the pane is too small.
- macOS: the keyboard no longer stays in a browser page once you use another pane, and closing a page that had the keyboard gives it back to the window.
- A link pasted into the browser's address field or given to `compi browse` loses text around it, such as quotes, `<…>`, a Markdown link, a dev server's `Local:` line or a trailing period. `compi browse` without a URL opens a blank pane with the keyboard in its address field.

## Updating

- **Running shells end.** The update restarts the local daemon after you consent. Daemon protocol 34 and workspace format 8 are unchanged, so your workspace carries over. Save your work first. Remote daemons are not restarted by the local updater.
- **Windows, from 0.3.0 or earlier:** run [Compi 0.4.1 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.4.1/Compi-0.4.1-Setup.exe) once; the older update helper cannot replace itself. From 0.3.1, update in the app.

## Limitations

- On macOS the browser pane, its keyboard fix and the PDF, audio and video previews have not yet been checked on screen. A page there hides while a floating pane overlaps it, failed loads show no failure notice, and loading shows only once content arrives.
- Animated GIF and WebP show their first frame only.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
