# Work with agents

Run your preferred agent in a Compi terminal. Keep its instructions, changes and the app it is building in the same workspace. You choose the agent and decide what to review and ship.

1. **Write the task.** Keep instructions or a spec in an ordinary Markdown file in your project. Open it in Files and use Edit / Read to write or read it. Give the agent the file's path; opening a file does not automatically add it to the agent's context.
2. **Run the work.** Start your agent in the project's directory. Keep another terminal beside it for builds, tests or a development server. Use tabs and splits to arrange the work without restarting its programs.
3. **Review the changes.** Open Changes or run `compi diff` to inspect the working-tree diff. Open a file to make a correction, then save it. The diff reads files on disk, not unsaved editor buffers. Check test output as well as the agent's description of its work.
4. **Inspect the result.** Start your app's development server, then open its address in a Browser pane. You can check the app beside the terminal that runs it. Automated browser verification uses your agent's own browser tools; opening a pane does not perform it.
5. **Return when needed.** In Settings → Notifications, choose how supporting programs notify you when they need attention or finish. Finished notifications are optional. Click a notification to return to its terminal. Programs must report status; ordinary terminal output does not trigger these events.

Closing Compi's window leaves the shells running. Restarting its server or rebooting ends those processes.

An agent can also use Compi's CLI to inspect panes, arrange the workspace and open results for you. Start with `compi help` or `compi --json help` for structured discovery. See [workspace control](USAGE.md#command-line-workspace-control), [editing and review](USAGE.md#files-and-projects-in-the-terminal) and [program status and notifications](USAGE.md#program-status) for details.
