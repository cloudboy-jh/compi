# 0.2.0

- Monitor: see what your machine is doing beside your terminal
- CPU, memory, disk and network at a glance, with the last hour of history
- Processes grouped by app, with their icons

## New

- Monitor opens from the palette as a pane that splits, floats and closes like any pane. It shows the machine Compi's server runs on, so a connected host shows that host.
- Overview has a card for CPU, memory, disk and network with its recent history, plus Memory by type and Memory by app. Click a card for its page. Pick 1m, 5m, 15m or 1h, or pause the graphs. Cards rearrange into three, two or one column as the pane's width allows.
- History is kept for the last hour even while Monitor is closed, so graphs are full when you open it.
- Processes lists every process with search, All / Apps / My processes / System filters and sortable columns. Group by app gathers each app's processes, helpers included on macOS, under one row with the app's icon and their totals. Rows hold still while the pointer is over the table.

## Fixes

- Programs that mark their own prompts, such as Pi, keep showing that they are working instead of losing their status mid-run.

## Limitations

- macOS doesn't let Compi read threads of other users' processes; they show a dash.
- App icons come from macOS and Windows apps; Linux hosts show initials.

## Updating

- Updating ends running shells. Save your work first.
