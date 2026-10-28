# 0.5.0

- Notifications when a program needs you or finishes, in Compi or from Windows
- A home page for blank browser panes: local servers running now and recent sites
- `compi` commands print readable tables and lines in a terminal
- `compi help` lists every command
- Updating ends running shells: the daemon restarts

## Notifications

- When a program in a terminal you are not using asks for a permission, a question's answer or a sign-in, Compi tells you. Programs report this through program status (OSC 7501), as they already do for the tab glyphs.
- **Settings → Notifications** (a new section, also `compi settings notifications`) picks the delivery: **Automatic** shows a card at the bottom right of the window while Compi is the active app and a system notification while it is in the background; **In-app only**, **System only** and **Both** do just that. **Send test** sends one with your settings.
- **Needs attention** is on by default; **Finished** (a program reports its work done) is off.
- Click a card or notification to go to that terminal. A card closes after 6 seconds; at most three show at once.
- **Sound** is off by default. Pick one of Windows' notification sounds (Default, Message, Mail, Reminder or Text message) and hear it with **Play**. It plays once per notification, and not while Windows is quiet (Do Not Disturb, presenting, a full-screen app).
- Each notification comes once, from one window, when the state changes; reopening Compi does not repeat old ones.
- The settings are also in `config.toml` under `[notifications]`; see [Configuration](CONFIGURATION.md#notifications).

## Browser

- A Browser pane without an address shows a home page. **Running here** lists local servers your terminals printed addresses for (such as a dev server's `Local:` line) that answer now, this tab's first. **Recent** lists the window's last five sites. Back from the first page returns home.

## Command line

- In a terminal, list commands print tables, `show` commands print `name: value` lines, and window actions say what the window did. Long names and paths are shortened to fit; IDs always print whole.
- Piped or redirected, commands print indented JSON as before. `--json` always prints the same single line, so scripts and agents should pass it.
- `compi help` lists every command by what it works on, with options, output rules and exit codes; `compi help pane` or `compi pane` lists a command's actions. `compi --json help` returns them all.
- An unknown command says so, suggests the closest one and exits 2 instead of opening the app.

## Updating

- **Running shells end.** The update restarts the local daemon after you consent: daemon protocol 35 (0.4.x used 34). Workspace format 8 is unchanged, so your workspace carries over. Save your work first. Remote daemons are not restarted by the local updater.
- **Windows, from 0.3.0 or earlier:** run [Compi 0.5.0 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.5.0/Compi-0.5.0-Setup.exe) once; the older update helper cannot replace itself. From 0.3.1, update in the app.

## Limitations

- macOS: notifications have no sound yet, and notifications have not yet been checked on a Mac. The browser pane and the PDF, audio and video previews have not yet been checked on screen there either.
- Animated GIF and WebP show their first frame only.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
