# compi.dev update to 0.9.4: handoff

Written 2026-11-23. compi.dev still describes **0.8.1**. The latest release is **v0.9.4** (published 2026-11-23 19:01 UTC). Since 0.8.1 there have been six releases (0.8.2, 0.9.0–0.9.4), and they change the product's main story. Bring the site up to v0.9.4.

## Where things are

- Site source: `github.com/cloudboy-jh/compi-marketing` (not checked out on this Mac; clone it). Static HTML/CSS/JS. `tools/generate-docs.mjs` generates `docs.html` from `content/<version>/` snapshots. Read its `README.md` first.
- Deploy: direct upload to the Cloudflare Pages project `compi-marketing` (`wrangler pages deploy dist --project-name compi-marketing --branch main`). Pushing to GitHub does not deploy. compi.dev is that project's custom domain. `/docs.html` returns 308 to `/docs`.
- Live today: the compi.dev landing page matches a fresh build of `compi-marketing@b35683e` byte for byte, so nothing is waiting to deploy.
- Product source: this repo. **The local checkout was 76 commits behind `origin/main`** (tags fetched, nothing pulled). Work from the `v0.9.4` tag (`git show v0.9.4:…`), not the local tree.
- Vault: `jacksmainvault/Compi` (see AGENTS.md) could not be found on this Mac. Read it if your machine has it, especially `References/` for scope rules.

## What changed in the product (marketing-relevant)

Sources: `gh release view vX.Y.Z -R cloudboy-jh/compi` for 0.8.2–0.9.3 (the repo's `release-notes.md` keeps only the latest), and `git diff v0.8.1 v0.9.4 -- docs/user`.

1. **Native chat with four agents** (0.9.0): OMP, Claude Code, Codex, OpenCode. Default agent setting, a per-project agent and model, `compi chat --agent omp|claude|codex|opencode`, and **New chat with …** in the palette. The site says "OMP only" in several places, which is now wrong.
2. **Chat first or Terminal first** (0.9.0): new tabs start as chats or terminals. Asked on first launch; changed in Settings → General.
3. **Threads** (0.9.0, 0.9.4): chats outlive their panes. Reopen them from **Threads** in the palette; `compi threads` and `compi threads delete ID`; renamed threads keep their names.
4. **Projects and sidebar** (0.9.0–0.9.2, 0.9.4): saved projects with icons; Add project (open a folder or clone a Git URL); **Home** project; tabs find their project; drag cards and projects; pane counts and hover lists on cards; Ctrl/Cmd+P finds files across the project.
5. **Sign-in and updates from the chat** (0.9.2): `/providers`, `/login`, `/logout`, `/update`; Settings → Agents shows versions and offers updates.
6. **Windows/WSL**: one install per agent across WSL and Windows, with an Ubuntu | Windows switch in Settings → Agents (0.9.4). **Automatic** shell picks PowerShell or WSL by folder (0.9.0). WSL chats tell the agent it is running on Windows (0.8.2). `compi screenshot` captures Windows app windows (0.8.2, Windows only).
7. **Settings reorganised**: a General page first, plus new Agents and Projects pages; Interface merged into Appearance.
8. **Updating**: from **0.8.0 or 0.8.1 on Windows, users must use Setup** (`Compi-0.9.4-Setup.exe`), because the in-app updater in those versions can hang and roll back. Every site visitor is on 0.8.1 or older, so this matters. Daemon protocol is now 44; workspace format changed 10 → 11 in 0.9.0.

Tagline and positioning are unchanged ("A workspace for you and your agents"; Compi is not an agent or an orchestrator).

## Tasks

### 1. Fix the docs importer (it fails on v0.9.4)

`node tools/import-docs.mjs <compi> v0.9.4` throws `Missing CLI entry catalogue`. In v0.9.4, `crates/compi-client/src/cli/help.rs`:
- renamed the array to `const CATALOGUE: &[Entry] = &[` (`ENTRIES` is now a `LazyLock` built from it);
- put `{programs}` and `{agents}` placeholders into `usage`/`detail`, which `Entry::naming_agents` fills at runtime. The values are documented at help.rs lines 9–12: `{programs}` = `omp|claude|codex|opencode`; `{agents}` = `omp (OMP), claude (Claude Code), codex (Codex) or opencode (OpenCode)`.

Update the importer to split on `const CATALOGUE: &[Entry] = &[` and expand both placeholders. Assert that no `{programs}`/`{agents}` text is left. Change the default ref and the usage text to `v0.9.4`.

New commands: `threads` and `screenshot`. The generator's `groups` list (generate-docs.mjs ~line 50) asserts that every command is grouped, so add them. help.rs now has its own `GROUPS` (Workspaces and tabs, Panes, Terminals, Views [+threads], Palette, Other apps [screenshot], Compi). Mirror those names.

### 2. Import and bump the version

- `node tools/import-docs.mjs ~/proj/compi v0.9.4`, set `version = '0.9.4'` in `generate-docs.mjs`, delete `content/0.8.1/`.
- `index.html`: the eyebrow (line 39), both download pairs (45–46, 167–168), the boundary line (80), `Get Compi …` (162) and the release-notes link (174). Asset names are unchanged: `Compi-0.9.4-Setup.exe`, `Compi-0.9.4-macOS-arm64.dmg`.
- `README.md` (site): replace the 0.8.1 references, and name compi.dev as production instead of `compi-marketing.pages.dev`.

### 3. Update the generator's hand-written docs text (`tools/generate-docs.mjs`)

A dry run with the v0.9.4 snapshots fails on one source reference: `bullet('Chat with OMP')`. The USAGE bullet is now **`Chat with an agent`**. After renaming it, everything else resolved. The prose below is hand-written and now wrong:
- **`chat` section (~111–121)**: lead line, the "Native chat supports OMP only" notice, OMP-only controls and the "OMP chat" palette name. Rewrite for four agents: New chat, Default agent, drafts, Threads, sign-in, `/update`. Use the USAGE bullets `Chat with an agent`, `New thread drafts`, `Default agent`, `Agents in WSL and Windows`, `Model providers, sign-in and updates from the chat (every agent)`, `OpenCode`, and the `compi chat`/`compi threads` paragraphs.
- **New section, "Projects and the sidebar"**: from USAGE bullets `Chat first or Terminal first`, `Add project`, `Sidebar`, `New tabs and threads`, `Sidebar cards`, `Drag sidebar cards`, `Find a file in the project`, and CONFIGURATION `## Projects`. Add it to the docs navigation.
- **`agents` section (~124–138)**: mention `compi screenshot` (Windows) for agents in WSL.
- **`updates` notice (~193)**: protocol 43 → 44. On Windows from 0.8.0/0.8.1, use Setup; give the link prominence. Keep "save your work first".
- **Troubleshooting (~203)**: "Native chat supports OMP only" is wrong. Replace with: install the agent where the chat runs; Settings → Agents shows what was found and marks missing agents **Not installed**.
- **`docsDescription` (~219)**: drop "native OMP chat".
- **Appearance and settings**: Interface is gone, so check every hand-written "Settings → Interface" mention (USAGE now says Settings → Appearance → Tab details and Settings → Notifications → Program status).

Do not edit `docs.html` directly; `npm run check` fails if it is out of date.

### 4. Rewrite the landing page copy (`index.html`)

- **01 / Agents** (lines 66–84): the heading "Native chat, for OMP" and the boundary line are wrong. New story: native chat for OMP, Claude Code, Codex and OpenCode (each keeps its own models, sessions and sign-ins; Compi supplies the view and keeps chats running in the daemon); sign in and update agents from the chat; threads persist and can be reopened. Other terminal agents still run in a terminal with the same CLI.
- Add **Chat first / Terminal first** and **projects + sidebar** where they fit: probably a short new section between Agents and Review, or folded into Terminals & CLI ("Arrange it around the task").
- CLI box (144–152): consider adding `compi chat --agent claude` and `compi threads`. Keep it at five or six lines.
- Meta `og:image:alt` and `twitter:image:alt` (17, 22) and the hero legend "OMP · native chat" (57): update them once the new screenshot exists.
- Keep: "Compi is the workspace, not an agent or an orchestrator", the signing caveats beside the downloads, and Windows x64 / Apple Silicon macOS 14+.
- Check every claim against the **v0.9.4** docs, not `main`.

### 5. Recapture screenshots on 0.9.4

`workspace-full.png` (and its crops `workspace-chat/diff/browser.png`) shows the 0.8.1 UI: no project sidebar, old chat and model picker. The product repo's `assets/compi-workspace.png` is the same file, and `assets/compi-social-preview.png` is identical in both repos at v0.9.4. Follow the site README's rules: recapture, never edit; update the crops and the hero legend's `--chat-span` and related variables in `style.css` together. A good capture would show a non-OMP agent's chat (or two agents), the project sidebar, a diff and a browser pane. Then replace the product repo's `compi-workspace.png` and the social preview in both repos so they stay identical. Needs the user's machine and a real session.

### 6. Fix the "Work with agents" guide at the source

`content/guides/AGENT_WORKFLOW.md` must be identical to the product repo's `docs/user/AGENT_WORKFLOW.md`. Right now:
- The site copy says "With OMP installed … **OMP chat**". Out of date.
- v0.9.4's product copy says "With **OMP or Claude Code** installed … **New chat**", which leaves out Codex and OpenCode, and later says "including OMP in a chat".

Fix the product repo file first (all four agents; "including agents in a chat"), commit it there, then copy it unchanged to the site.

### 7. Turn off Cloudflare email obfuscation for compi.dev

On the live `/docs`, the code samples `compi --connect dev@example.com:2222` and `compi-probe --connect dev@example.com workspace` show as `[email protected]` and need `email-decode.min.js`. Cloudflare's Scrape Shield → Email Address Obfuscation does this on the compi.dev zone; the built `dist/docs.html` is correct. Disable it for the zone (or with a configuration rule for compi.dev). Check that `curl -sL https://compi.dev/docs | grep -c cdn-cgi/l/email-protection` prints 0.

## Check and ship

1. `npm ci && npm run build && npm run check` in the site repo.
2. Serve `dist/` (`python3 -m http.server 8791 --bind 127.0.0.1 --directory dist`) and look at both pages in a browser: desktop and phone widths, both appearances, every docs anchor the landing page links to (`#get-started`, `#chat`, `#files`, `#browser`, `#agents`, `#cli-reference`, plus the new projects section).
3. `curl -sIL -o /dev/null -w '%{http_code}\n'` both download links must print 200.
4. Ask the user before deploying. Then deploy and compare `curl -s https://compi.dev/ | shasum` with `shasum dist/index.html`.
5. Commit in the site repo; commit the AGENT_WORKFLOW and screenshot changes in this repo separately.
