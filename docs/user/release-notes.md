# 0.4.0

- Browser pane: open a web page or local dev server beside its terminal
- Images, SVG, PDF, audio and video open in file tabs
- Text files over 1 MiB open read-only, a part at a time
- Markdown has Edit / Read, remembered per file, keeping your place
- Updating ends running shells: the daemon must restart

## Browser pane

- `compi browse URL` (or **Open browser** in the palette) opens a page in a pane beside the terminal; `--down` splits below. It splits, floats and closes like any pane and comes back after a restart.
- Toolbar with Back, Forward, Reload/Stop and an address field (`Ctrl+L`, `Cmd+L` on macOS, or `F6`). Pages that fail to load show why, with **Retry**.
- `compi window list` reports each shown page's address, title, loading state and error.
- Uses the system web view (WebView2 on Windows, WebKit on macOS) with one profile per Compi instance. Only `http`, `https`, `file` and `about:blank` addresses open.

## File previews

- Images (PNG, JPEG, GIF, WebP, BMP, ICO and others) open as images, with zoom about the pointer, drag to pan, Fit and 100%. SVG files get a **View** mode beside Edit.
- PDF, audio and video open in the system web view: PDFs in its viewer, audio and video in a player. Files stream in pieces, so size does not matter; playback pauses when the tab is hidden, and closing the tab releases it.
- Text files too large for the editor (over 1 MiB) show read-only 1 MiB at a time, with Start, Previous, Next and End, search within the part, and a notice when the file changes on disk.
- Files the preview cannot show say what they are, for example `2.0 MB · Video file`.

## Markdown

- **Edit / Read / Diff** switch on every Markdown tab, and **Read Markdown** (`Ctrl+E`, `Cmd+E` on macOS; a terminal keeps the key).
- Switching keeps your place: the line at the top of one mode stays at the top of the other.
- Each window remembers which files you last read, so a file reopens in the mode you left it. `compi notes` opens in Read, `compi edit` in Edit.

## Fixes

- Resizing a window no longer jumps a scrolled-back terminal to the bottom, including when a program such as omp reprints its text at the new width.
- The image inspector's wheel zoom is gentler (about 1.4× a notch instead of over 2×).

## Updating

- **Running shells end.** Daemon protocol 34 and workspace format 8 replace 0.3.x's protocol 32 and format 7, so the update restarts the local daemon after you consent. Save your work first. Remote daemons are not restarted by the local updater.
- A 0.3.x daemon refuses a workspace saved by 0.4.0. Do not downgrade an existing workspace without a backup.
- **Windows, from 0.3.0 or earlier:** run [Compi 0.4.0 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.4.0/Compi-0.4.0-Setup.exe) once; the older update helper cannot replace itself. From 0.3.1, update in the app.

## Limitations

- On macOS the browser pane and the PDF, audio and video previews have not yet been checked on screen. A page there hides while a floating pane overlaps it, failed loads show no failure notice, and loading shows only once content arrives.
- Animated GIF and WebP show their first frame only.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
