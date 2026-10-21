# 0.3.1

- Windows updates install and reopen Compi
- Failed updates recover the previous windows instead of silently stopping
- Existing Windows installs: run 0.3.1 Setup once to replace the old updater

## Fixes

- The Windows updater no longer mistakes its own background task for another installation because Windows reports the executable path in two different forms.
- Installation ownership is checked before Compi closes or a consented daemon stops.
- An authenticated update failure before activation now records the error and reopens the previous saved views when safe. Failed new-build attachment restores the previous version without rolling back underneath a still-running replacement GUI.
- `compi update` waits for the new GUI to confirm attachment before reporting an installed update. Starting the helper, rolling back or timing out is not success.
- Retrying a failed CLI installation re-verifies the retained package as a fresh attempt instead of immediately reporting the previous attempt's error.
- Reopened windows show the final update result instead of leaving the install state unresolved.

## Updating on Windows

- **Upgrading from 0.3.0 or earlier: download and run [Compi 0.3.1 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.3.1/Compi-0.3.1-Setup.exe) once.** The older installed update helper must be replaced by Setup; downloading a new payload through the broken updater does not replace it. Save your work before Setup and review any prompt to close Compi or end running shells.

- Subsequent updates use the repaired helper to install and reopen Compi. Normal consent still applies when a daemon restart would end shells; the local updater never restarts remote daemons.

- Daemon protocol 32, workspace format 7 and GUI control handoff 12 are unchanged from 0.3.0. Upgrading from 0.2.x still requires the protocol/workspace transition introduced in 0.3.0; do not downgrade an existing workspace without a backup.

## Limitations

- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
- Physical-input, macOS visual/IME and SSH note-file qualification from 0.3.0 remain open.
