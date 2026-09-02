![Compi](assets/compi-readme.png)

# Compi

Compi is a native Windows terminal for persistent WSL2 sessions. The window is a client, while a per-user daemon owns each shell and its terminal state. Closing Compi detaches the window without stopping the work inside it.

## Product

- **Persistent by default.** Reopen Compi and reconnect to live shells, scrollback, and screen state.
- **Built for WSL2.** New sessions start in the Linux user's home directory in the default WSL2 distribution.
- **Native Windows client.** GPUI renders the terminal, tabs, titlebar, selection, images, and window controls.
- **Multiple independent sessions.** Create, detach, switch, and reconnect without adding an in-terminal multiplexer.
- **Modern terminal behavior.** Unicode, ANSI styling, alternate screens, local scrollback, mouse input, bracketed paste, and Kitty graphics are part of the core terminal model.

## How it works

```text
GPUI client  <->  per-user Compi daemon  <->  ConPTY  <->  WSL2 Bash
```

The daemon is the authority for session lifecycle and terminal state. The client can disappear and reconnect without becoming the owner of the shell process.

## Status

Compi is a pre-release Windows application with the core persistent-terminal experience implemented and available for dogfooding. The remaining release path is installer and daemon supervision, lifecycle hardening, broader terminal compatibility, performance validation, and repeatable signed Windows releases.

See [NEXT_STEPS.md](NEXT_STEPS.md) for the active roadmap.
