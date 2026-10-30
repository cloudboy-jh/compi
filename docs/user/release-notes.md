# 0.5.1

- macOS: notifications play a sound, with macOS's alert sounds to choose from
- **Focused pane**: the pane you're in notifies too once you've stepped away
- Tab tooltips state program status once
- Windows: plain Ctrl+= zooms in
- Updating ends running shells: the daemon restarts

## Notifications

- On macOS, **Sound** offers Default and macOS's alert sounds (Basso, Blow, Bottle, Frog, Funk, Glass, Hero, Morse, Ping, Pop, Purr, Sosumi, Submarine, Tink), with **Play** to hear one. The sound comes with the system notification, or with the card in Compi, once per notification. Focus and Compi's **Play sound for notifications** switch in System Settings silence it, except for a card's sound while Compi is in the background with **In-app only**.
- A `config.toml` shared between Windows and macOS keeps either system's sound; the other system plays its default sound.
- **Focused pane** (Settings → Notifications → Events, `notifications.focused_pane_after`): the pane you're in counts as in use only while you're active. After 30 seconds without a key press, click or scroll in its window (by default), it notifies too. **Never** keeps it quiet while focused.

## Fixes

- Hovering a tab shows its title and its programs' records without repeating the state, and a tab with one pane no longer repeats the pane's name.
- Windows: Ctrl+= zooms in like Ctrl+Shift+=, as in browsers and editors.

## Updating

- **Running shells end.** The update restarts the local daemon after you consent. Daemon protocol 35 and workspace format 8 are unchanged, so your workspace carries over. Save your work first. Remote daemons are not restarted by the local updater.
- **Windows, from 0.3.0 or earlier:** run [Compi 0.5.1 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.5.1/Compi-0.5.1-Setup.exe) once; the older update helper cannot replace itself. From 0.3.1, update in the app.

## Limitations

- macOS: clicking a system notification has not yet been checked on a Mac. The browser pane and the PDF, audio and video previews have not yet been checked on screen there.
- Animated GIF and WebP show their first frame only.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
