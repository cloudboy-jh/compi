# 0.1.5

- Updates no longer get stuck after the daemon's console window is closed

## Fixes

- Closing the console window that Compi's background service opens at sign-in no longer stops its supervisor.
- An update no longer refuses to run when the supervisor has stopped but its daemon is still running.

## Updating

- Updating restarts the Compi daemon, which ends running shells. Save your work first.
- Stuck on 0.1.3 with "cannot match target supervisor lifecycle identity"? Run `& "$env:LOCALAPPDATA\Programs\Compi\versions\0.1.3\compi-daemon.exe" --shutdown` in PowerShell, then reopen Compi and update. This ends shells in the installed Compi.
