# 0.9.3

- The model picker works again: choose any provider and model
- Updating restarts the local daemon and ends running shells and chats

## Fixes

- In 0.9.2, pressing the provider icons or the left side of the model list closed the model picker instead of choosing. The picker now opens from the top of the message box, lined up with it, and takes every click.

## Updating

- **Save your work first.** The update restarts the local daemon after you consent, ending its running shells and chats. It does not restart remote daemons. Daemon protocol stays 43.
- **From 0.8.0 or 0.8.1 on Windows, use [Compi 0.9.3 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.9.3/Compi-0.9.3-Setup.exe).** Those versions' in-app updater can hang and roll back. From 0.3.0 or earlier, use Setup once to replace the old update helper.
- Windows packages are not publisher-signed. The macOS app may be ad-hoc signed and not notarized. Signed update metadata does not replace operating-system publisher signing.
