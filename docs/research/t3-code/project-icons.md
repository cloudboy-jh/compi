# T3 Code project icons

How T3 Code picks, draws, overrides and caches a project's icon. All paths are from the T3 Code repository root. Tailwind sizes: `size-3` = 12 px, `size-3.5` = 14 px, `size-4` = 16 px, `size-6` = 24 px, `size-8` = 32 px, `size-12` = 48 px.

## Summary
- **A project has three icon layers, checked in this order:** a chosen icon (`projectIcon`: Lucide icon, emoji or monogram), a chosen image file (`faviconPath`), then the automatic image found in the repository. If none of them resolves, the client draws a coloured two-letter monogram derived from the project title (`apps/web/src/components/ProjectFavicon.tsx:35-151`).
- **Automatic detection runs on the server.** It reads only files: an override path, `t3.json` `iconPath`, a fixed list of 23 favicon/logo paths, then `<link rel="icon">` tags in 7 HTML or root-route files. It does not use `package.json`, framework logos, git remotes or avatars (`apps/server/src/project/ProjectFaviconResolver.ts:40-80,278-321`).
- **The purple "CI" for compi is the monogram fallback.** "compi" is one word, so the badge uses its first and last letters. The colour hashes to `purple`, index 14 of an 18-colour Tailwind palette (`apps/web/src/projectIdentity.ts:13-34`). I checked this by running the same algorithm.
- **"Choose file" stores a path, not a copy of the file.** The path is workspace-relative, or absolute when picked through the desktop file dialog. The server serves the file through a signed asset URL named after its SHA-256 (`apps/server/src/assets/AssetAccess.ts:727-835`).
- **Images are cached three times:** a server lookup cache (10 min for a hit, 1 min for a miss), a signed URL that stays the same for 30 min, and a client IndexedDB cache of small data URLs (max 128 entries, 1 MiB). I found no file watcher.

## Files
- `apps/server/src/project/ProjectFaviconResolver.ts`: automatic lookup, candidate lists, server cache.
- `apps/server/src/project/ProjectEnrichmentService.ts`: background lookup for full `Project` reads.
- `apps/server/src/assets/AssetAccess.ts`, `apps/server/src/assets/icns.ts`: signed URL minting, `.icns` → PNG.
- `apps/server/src/ws.ts:3081-3108`: the `assets.createUrl` handler for `project-favicon`.
- `packages/contracts/src/project.ts:49-205`: `ProjectIconColor`, `ProjectIconOverride`, `ProjectFaviconPath`, `ProjectUpdatePayload`.
- `packages/contracts/src/t3ProjectFile.ts:84-91`: the `iconPath` key in `t3.json`.
- `packages/shared/src/projectFavicon.ts`: allowed extensions, cache keys, the "missing" marker.
- `packages/client-runtime/src/state/assets.ts:326-398`, `packages/client-runtime/src/projectFaviconCache.ts`: client URL atom and image cache.
- `apps/web/src/assets/projectFaviconCache.ts`: IndexedDB storage and canvas downscale.
- `apps/web/src/components/ProjectFavicon.tsx`, `ProjectMonogram.tsx`, `apps/web/src/projectIdentity.ts`, `apps/web/src/projectIconColors.ts`: rendering.
- `apps/web/src/components/settings/ProjectSettingsPanel.tsx:598-643,736-758`, `ProjectIconPickerDialog.tsx`, `ProjectFaviconPickerDialog.tsx`, `ProjectFaviconThumbnail.tsx`, `apps/web/src/projectIconOptions.ts`: settings UI.
- `docs/user/project-settings.md:276-286`: user documentation.

## Facts

### Automatic icon: lookup order (server)
`resolvePathUncached(cwd, faviconPath?)` (`ProjectFaviconResolver.ts:278-321`):
1. **Stored override `faviconPath`**, if one is set. It may be absolute ("filesystem" scope, `:156-165,292-297`).
2. **`t3.json` `iconPath`**, a workspace-relative path described as "Checked before T3 Code's built-in icon locations" (`:299-309`, `packages/contracts/src/t3ProjectFile.ts:84-91`). `t3.json` is the per-repo project file (`t3ProjectFile.ts:8`).
3. **Well-known files, first match wins** (`ProjectFaviconResolver.ts:40-64`): `favicon.svg|ico|png`, `public/favicon.svg|ico|png`, `app/favicon.ico|png`, `app/icon.svg|png|ico`, `src/favicon.ico|svg`, `src/app/favicon.ico`, `src/app/icon.svg|png`, `assets/icon.svg|png`, `assets/logo.svg|png`, `.idea/icon.svg`, `Resources/AppIcon.icns`, `AppIcon.icns`.
4. **Icon links in source files** (`:66-80,200-260`):
   - Files checked: `index.html`, `public/index.html`, `app/routes/__root.tsx`, `src/routes/__root.tsx`, `app/root.tsx`, `src/root.tsx`, `src/index.html`.
   - Only the first 128 KiB of each file is read (`:19`).
   - It looks for `<link rel="icon"|"shortcut icon" href=…>`, or a JS object with `rel: "icon"` and `href: "…"`, which covers TanStack/Remix route heads (`:119-131`).
   - The `href` loses its query string and leading `/`. It is then tried as `public/<href>` and as `<href>` (`:150-154`).
- **Probes run in parallel, but the result follows list order**, so a slow low-ranked probe never delays a hit (`:262-276`).
- **Everything must stay inside the workspace:** a candidate outside the workspace root is skipped (`WorkspacePathOutsideRootError`, `:171-177`). Only regular files count (`:193`).
- **Not used:** `package.json`, framework or language logos, git remotes, owner avatars. The lists above are the whole lookup.
- **Image types allowed for serving:** avif, bmp, gif, ico, heic, heif, jpeg, jpg, png, svg, tif, tiff, webp, plus icns (`packages/shared/src/projectFavicon.ts:5-10`, `packages/shared/src/mediaPreview.ts:22-36`).
- **`.icns`:** browsers can't draw it, so the server extracts an embedded PNG. It takes the smallest one at least 96 px wide, or the largest if none is that big (`AssetAccess.ts:278-296`, `apps/server/src/assets/icns.ts`).

### Automatic icon: where it runs, caching, refresh
- **Server lookup cache:** 512 entries keyed by `faviconPath\0cwd`. A hit is kept 10 min and a miss 1 min. Errors are not cached, except a missing workspace root (1 min) (`ProjectFaviconResolver.ts:18-21,323-339`).
- **Each cached hit is re-checked with `stat`.** If the file has gone, the lookup runs again (`:341-367`).
- **The enrichment service caches separately:** 4 workers and a 15 min TTL ("icons change rarely") (`ProjectEnrichmentService.ts:18-24,142-148`). It fills `Project.faviconPath` as `row.faviconPath ?? enrichment.faviconPath` (`apps/server/src/project/ProjectService.ts:202`). The sidebar's shell snapshot gets only `repositoryIdentity` from enrichment (`ProjectService.ts:593-599`), so on the client `faviconPath` means the user's override.
- **Cache invalidation happens only on:** project create, root change, `faviconPath` change, delete, and clone finished (`ProjectService.ts:412,453-455,588`; `ws.ts:2912-2915`; `AssetAccess.ts:738-740`). I found no file watcher, so a new or edited favicon shows up only after the TTLs or a URL refresh [INFERENCE].
- **URL minting** (`assets.createUrl`, `packages/contracts/src/rpc.ts:459`):
  - The client sends resource `{_tag: "project-favicon", cwd, path?}` (`packages/contracts/src/assets.ts:58-62`).
  - The server looks up the project by root and passes the stored override and clone phase. A clone still in progress always reports "missing" (`ws.ts:3081-3108`, `AssetAccess.ts:741-745`).
  - A found file gets a signed URL whose file name is `v<sha256 of bytes>-<basename>`. With no file, the name is the marker `project-favicon-missing` (`AssetAccess.ts:810-834`, `projectFavicon.ts:3`).
  - Favicon tokens expire on a 30 min bucket boundary plus one, so the URL stays the same for up to 30 min (`AssetAccess.ts:72,897-903`).
- **Client URL atom:** one per `[environmentId, cwd, faviconPath]`.
  - It refreshes every 30 min, is dropped after 60 min idle, and asks again whenever a clone's phase changes (`packages/client-runtime/src/state/assets.ts:30-31,351-379`).
  - The "missing" marker becomes `null`, which means draw the monogram (`:390-394`).
  - Choosing a file or resetting forces a re-ask through `refreshProjectFaviconUrl` (`apps/web/src/state/assets.ts:40-51`, `ProjectSettingsPanel.tsx:338-346`).
- **Client image cache** (`packages/client-runtime/src/projectFaviconCache.ts:12-16,172-218`):
  - Images are fetched and stored as data URLs, keyed by the sha file name, so unchanged bytes are never fetched again.
  - Sources are capped at 4 MiB. A data URL over 32 KiB is downscaled to a 96 px WebP at quality 0.85, then 48 px if still too big (`apps/web/src/assets/projectFaviconCache.ts:48-71`).
  - An SVG over 32 KiB stays a remote URL (`packages/client-runtime/src/projectFaviconCache.ts:85-117`).
  - LRU limits: 128 entries and 1 MiB total, persisted in IndexedDB `t3code:project-favicons` / `images` (`apps/web/src/assets/projectFaviconCache.ts:8-10`).
  - `peek` serves the cached image right away on startup.

### Monogram fallback
- **Letters** (`apps/web/src/projectIdentity.ts:13-25`):
  - The name is NFKC-normalised and trimmed, then split into runs of letters and digits.
  - The first glyph of the first word comes first. The second glyph is, in order: the first digit after it in the same word; else the first glyph of the last word; else the last glyph of the first word. Both are uppercased and cut to 2 graphemes. An empty name gives `PR`.
  - Examples: `Nebula` → `NA`, `Silver Orchard` → `SO`, `M7 Forge` → `M7` (`docs/user/project-settings.md:284-286`); `compi` → `CI`, `t3code` → `T3`.
- **Colour** (`projectIdentity.ts:27-34`):
  - Hash: `index = (index*31 + codePoint) % 18` over the lowercased name.
  - Result picks from `gray, red, orange, amber, yellow, lime, green, emerald, teal, cyan, sky, blue, indigo, violet, purple, fuchsia, pink, rose` (`packages/contracts/src/project.ts:49-68`).
  - Computed examples: compi → purple, Nebula → red, t3code → teal.
- **Colour tokens:** text uses `text-<c>-600` in light mode and `dark:text-<c>-400` in dark mode. Picker swatches use `bg-<c>-500` (`apps/web/src/projectIconColors.ts:4-117`). These are stock Tailwind v4 colours (`apps/web/package.json:118`); `apps/web/src/index.css` does not override them.
- **Tile** (`apps/web/src/components/ProjectMonogram.tsx:5-48`):
  - Default size 16 px: an inline-flex `size-4` span wrapping a 16×16-viewBox SVG with `rounded-[25%]`.
  - The fill is `color-mix(in srgb, currentColor 14%, transparent)`, a 14 % tint of the text colour.
  - The text sits at x 8, baseline 10.8, centred. Font size is 8.25 units, weight 700, in the monospace stack (`ui-monospace, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace`, `apps/web/src/index.css:174-175`).
  - The text is stretched to fit with `textLength`: 6 for one glyph, 12 for two (`lengthAdjust="spacingAndGlyphs"`).
- **Fallback chain inside `ProjectFavicon`** (`ProjectFavicon.tsx:118-151`):
  1. If the project has a title, draw the monogram. Project titles are always non-empty (`project.ts:156`), so this is the case in practice.
  2. Else, if the project has an emoji, draw it at 80 % of the box.
  3. Else draw Lucide `FolderCode` with `text-icon-muted`.
- **While an image loads, the monogram stays on screen.** A hidden `<img>` preloads the image and swaps it in on load. Data URLs show at once (`:153-201`).
- **Image style:** `size-3.5` with `rounded-[25%] object-contain` (`:185`).
- **Mobile has its own copy** of the monogram helpers (`projectAutoMonogram` in `apps/mobile/src/lib/projectIcon.ts:42-49`); they are not in a shared package. I did not compare it line by line [INFERENCE that it matches].

### "No project" icon
- The scratch "No project" project is given `{kind: "lucide", name: "message-square-dashed", color: "gray"}` once, when it is created. It is never written again, so a user's own choice survives (`apps/server/src/project/ManagedProjectFolders.ts:334-343`).
- **Look:** Lucide `MessageSquareDashed` (a speech bubble with a dashed outline) in gray-600, or gray-400 in dark mode.
- **Reset** gives this icon back to the scratch project (`apps/web/src/components/settings/ProjectSettingsPanel.logic.ts:9-14,38-53`).
- The command palette's "No project" item and the empty-state hero use the same glyph (`apps/web/src/components/CommandPalette.tsx:928-929`, `NoProjectsHero.tsx:52`).

### Settings → Projects → Project icon row
- **Description** (`ProjectSettingsPanel.tsx:598-608`): `name · color` for Lucide, `text · color` for a monogram, the emoji itself, the override path, or "Automatic".
- **Preview:** `ProjectFavicon` at `size-6` (24 px), followed by two `size="sm" variant="outline"` buttons, "Choose icon" and "Choose file", with an 8 px gap (`:618-642`).
- **Reset:** a reset button appears when any member checkout is not on its default (`:609-617`). The default is `{faviconPath: null, projectIcon: null}`, meaning Automatic, or the scratch icon for "No project" (`ProjectSettingsPanel.logic.ts:38-40`). The docs call this "Choose **Automatic** to let T3 Code detect an icon again" (`docs/user/project-settings.md:279-280`).
- **The two choices exclude each other:** choosing an icon saves `{faviconPath: null, projectIcon}`, and choosing a file saves `{faviconPath, projectIcon: null}` (`ProjectSettingsPanel.tsx:744,755`).
- **Scope:** a save writes to every checkout in the project group, across environments, through one `projects.mutate` `project.update` per member. If any environment is disconnected, the save fails with "Connect … and try again" (`apps/web/src/hooks/useProjectGroupUpdate.ts:45-75`, `ProjectSettingsPanel.tsx:305-308,333-349`). The docs say the choice "appears on connected clients" (`project-settings.md:278-279`).

### "Choose icon" dialog
Source: `apps/web/src/components/settings/ProjectIconPickerDialog.tsx`.
- **Layout:** a `sm:w-[32rem]` dialog titled "Choose project icon", with the description "Choose an icon, emoji, or monogram." A segmented toggle switches between **Icons / Emoji / Monogram** (`:62-150`).
- **Starting values:** the current icon if there is one. Otherwise `folder-code`, the automatic colour, the automatic letters, and the emoji 💻 (`:34,90-101`).
- **Colour row** (Icons and Monogram only): 18 round 24 px buttons, each holding a 16 px `bg-<c>-500` dot. The selected one gets `border-foreground/64` (`:152-175`).
- **Icons:** the whole Lucide set (`lucide-react ^0.564.0`, `apps/web/package.json:75`).
  - With an empty search, 24 "popular" icons show: folder-code, code-2, terminal, globe-2, server, database, bot, sparkles, smartphone, monitor, cloud-cog, package, book-open, flask-conical, shield-check, rocket, gamepad-2, music, image, shopping-bag, git-branch, workflow, wrench, layers-3 (`apps/web/src/projectIconOptions.ts:6-31`).
  - Search: the text is turned into kebab-case and substring-matched against all Lucide names, capped at 60 (`:66-70`). The placeholder reads "Search all Lucide icons".
  - Grid: 8 columns, 10 at `sm`, 4 px gaps, square `rounded-md` cells. Icons are 20 px and drawn in the selected colour. The selected cell gets `border-border bg-accent`. The grid scrolls at a max height of 256 px with a fade (`ProjectIconPickerDialog.tsx:177-211`).
  - The full icon set loads as one lazy chunk (`apps/web/src/lib/lucideIconByName.ts:1-27`, `ProjectFavicon.tsx:16-25`).
- **Emoji:** 30 preset emoji in the same grid at `text-xl`. A "Paste an emoji" field keeps the first grapheme, and only if it is pictographic, a flag or a keycap (`projectIconOptions.ts:33-83`, `ProjectIconPickerDialog.tsx:236-273`).
- **Monogram:**
  - A 48 px live preview and a "Letters" input with the hint "One or two letters or numbers." The letters are NFKC-normalised and uppercased (`ProjectIconPickerDialog.tsx:107-108,212-235`).
  - Schema: letters and digits only, max 32 chars (`project.ts:79-82`). The server rejects more than 2 graphemes (`apps/server/src/orchestration-v2/ProjectCommands.ts:175-180`).
- **Footer:** Cancel and "Save icon". Nothing is saved until Save (`ProjectIconPickerDialog.tsx:276-283`).

### "Choose file" dialog
Source: `apps/web/src/components/settings/ProjectFaviconPickerDialog.tsx`.
- **Form:** it reuses the command palette (`panelSize="tall-list"`, placeholder "Search image files…", footer "Select icon"). Results are grouped under the project name (`:93-163`).
- **Search:** results come from the server-side workspace search with `imageOnly: true`, which filters with `isProjectFaviconPath` (`:59-65`, `apps/server/src/workspace/WorkspaceSearchIndex.ts:206-210`).
- **Rows:**
  - Each row has a 32 px thumbnail, fetched lazily only once the row scrolls into view (`IntersectionObserver`) (`ProjectFaviconThumbnail.tsx:1-56`).
  - The image is drawn at most 32 px, `rounded-sm object-contain`.
  - Until it loads, the row shows the Pierre file-type icon.
- **Empty states:** "Indexing project files…", "Searching project files…", "No image files found.", "No matching image files." (`ProjectFaviconPickerDialog.tsx:26-40`).
- **"Open in Finder/Explorer"** opens the native file dialog through the desktop IPC `desktop:pick-project-favicon`, with an "Images" filter of `PROJECT_FAVICON_EXTENSIONS` (`apps/desktop/src/ipc/methods/window.ts:304-328`). It returns an absolute path.
  - It is offered only when every checkout is on the primary local environment.
  - On a Windows client it is offered only when the project path is a Windows path, so a WSL project gets no native dialog (`ProjectFaviconPickerDialog.tsx:41-43`, `ProjectSettingsPanel.tsx:294-302`).
- **Size limits:** none at pick time, because the file is referenced in place. Display limits come from the client cache (4 MiB source, 32 KiB stored thumbnail).
- **Storage:** the path goes in `projection_projects.favicon_path` and the chosen icon as JSON in `project_icon_json` (`apps/server/src/persistence/Migrations/040_ProjectionProjectFaviconPath.ts`, `047_ProjectionProjectIcon.ts`, `apps/server/src/orchestration-v2/ProjectStore.ts:40-71`).
- **Path checks:** the `project.update` payload only requires a non-empty string (`project.ts:195`). The asset path, and serving, require an image extension (`project.ts:98-101`, `AssetAccess.ts:763-765`).
- **Across environments:** a relative path is applied to each checkout and resolved against that checkout's root [INFERENCE from the per-member write plus per-root resolution]. An absolute path picked on one machine probably resolves nowhere else, which is likely why the native dialog is limited to the local environment [INFERENCE].

### Where icons appear, and at what size
| Place | Size | Source |
|---|---|---|
| Sidebar thread row (v2) | 16 px; 40 % opacity and grayscale when settled and not active, full on hover | `apps/web/src/components/sidebar/SidebarThreadRow.tsx:1827-1837` |
| Sidebar thread row (compact / multiline) | 16 px; dimmed the same way when inactive | `SidebarThreadRow.tsx:1303-1315,1506-1510` |
| Thread hover card | 12 px | `SidebarThreadRow.tsx:303-306` |
| Project folder header (group by project) | 16 px | `apps/web/src/components/sidebar/SidebarProjectFolderHeader.tsx:91` |
| Sidebar draft block | 16 px | `apps/web/src/components/sidebar/SidebarDraftBlock.tsx:116` |
| Project filter trigger | one project: 16 px. Several: the first two at 14 px, overlapped with `-ml-1` and a 2 px `ring-sidebar` cut-out. None: `ListFilterIcon` | `apps/web/src/components/Sidebar.tsx:3040-3061` |
| Project filter list rows | 16 px | `Sidebar.tsx:3138-3144` |
| Chat header breadcrumb | 14 px, before the project name. Clicking it starts a new thread there | `apps/web/src/components/chat/ChatHeader.tsx:270-285` |
| New-thread project picker | 16 px | `apps/web/src/components/chat/DraftHeroHeadline.tsx:284` |
| Command palette project items | 16 px, `text-icon-muted` | `apps/web/src/components/CommandPaletteProjectItems.tsx:33`, `CommandPalette.logic.ts:43` |
| Palette thread subtitle | 12 px | `apps/web/src/components/ThreadCommandSubtitle.tsx:69` |
| Thread link chip in markdown | `1em` | `apps/web/src/components/ThreadContextChip.tsx:54` |
| PR list project filter | 14 px | `apps/web/src/components/pullRequest/PullRequestListFilters.tsx:77` |
| Settings scope pickers and notice | 14 px / 16 px | `SettingsScopeSentence.tsx:414,442`, `SettingsScopeNotice.tsx:101` |
| Settings project icon row | 24 px | `ProjectSettingsPanel.tsx:620` |
| Archived threads | component default (14 px image or Lucide icon, 16 px monogram) | `apps/web/src/components/settings/ArchivedThreadsPanel.tsx:248` |

### Data model and RPCs
- `Project.faviconPath: string | null` and `Project.projectIcon: ProjectIconOverride | null` (`project.ts:154-171`). The orchestration shell carries the same two fields (`packages/contracts/src/orchestrationProject.ts:20-21`).
- `ProjectIconOverride` is one of (`project.ts:84-109`):
  - `{kind: "lucide", name, color}`, where the name is kebab-case and at most 64 chars;
  - `{kind: "emoji", emoji}`, at most 32 chars;
  - `{kind: "monogram", text, color}`.
- **Compatibility:** older `lucide` + `monogramText` records decode as monograms, and unknown kinds decode as `null` (`project.ts:111-152`).
- **Writes:** `projects.mutate` with `{type: "project.update", projectIcon?, faviconPath?}` (`packages/contracts/src/rpc.ts:448`, `project.ts:189-218`). This emits `project.meta-updated` (`ProjectCommands.ts:196-212`).
- **Reads:** image URLs come from `assets.createUrl` with the `project-favicon` resource. The picker uses `projects.searchEntries` with `imageOnly`.
- **Repo-level setting:** `t3.json` → `iconPath`.

## Implications for Compi
- **Run the lookup in the Rust daemon, keyed by folder path.** Port T3's order directly:
  1. user override;
  2. a repo file key, such as an `iconPath` in a Compi project file [INFERENCE];
  3. the 23 well-known paths;
  4. a scan of the first 128 KiB of `index.html` and root-route files with T3's two regexes.
  
  Return an absolute path plus a SHA-256 revision. Cache hits for 10 min and misses for 1 min. Re-`stat` a cached hit before using it.
- **WSL and Windows:** the daemon should do the lookup where the folder lives. For a `\\wsl$\…` or WSL-side path, read through the same path layer the daemon already uses for that folder, not through the Windows UI process. T3 avoids this case altogether by not offering the native picker for WSL projects on a Windows client.
- **Send bytes or a revisioned handle to the GPUI client, not a path.** Keep a small on-disk thumbnail cache keyed by `(folder, revision)`, at about 96 px and 32 KiB, so icons appear instantly at startup.
- **Image formats:** decode raster images with the `image` crate the client already has (ico is enabled, `crates/compi-client/Cargo.toml:57`). SVG needs `resvg`/`usvg` or GPUI's own SVG path [INFERENCE]. Extract `.icns` PNGs on the daemon side as T3 does.
- **Monogram fallback:** copy `projectIdentity.ts` exactly so compi stays purple "CI". That means NFKC, first glyph + (digit | last word's first glyph | last glyph), and `(i*31+cp)%18`. Draw a 16 px square with 25 % corner radius, a 14 % tint of the colour as fill, bold mono letters at about 8.25/16 of the box, and light/dark shades of the Tailwind 600/400 colours.
- **Overrides:** store `projectIcon` (Lucide name + colour, emoji, or monogram + colour) and `faviconPath` in the daemon's project record, setting them as mutually exclusive, with "Automatic" being both null. Lucide is easy to vendor as SVGs. Shipping the 24 popular names plus search over the full set matches T3.
- **Refresh without a watcher:** invalidate on project add, root change, override change, and clone done, and let TTLs handle the rest. A cheap Compi improvement would be re-resolving when a terminal or agent writes one of the candidate paths [INFERENCE].
- **Sizes to copy:** 16 px in sidebar rows and lists, 14 px in breadcrumbs and stacked filter chips, 12 px in hover cards and subtitles, 24 px in the settings row. Dim icons on settled rows to 40 % opacity and grayscale.
