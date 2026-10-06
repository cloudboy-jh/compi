# Compi release flow

Small commits, deliberate release batches. A version is an exact source snapshot, not one version per feature or commit.

## Working rhythm

Build → commit → choose a useful batch → package and check → publish → post the clips.

- Commit coherent changes separately to `main` when authorized. Do not squash unrelated work just to make a release.
- Release a useful, finished batch without waiting for the entire backlog. A significant bug fix can justify its own release.
- Keep three statuses distinct: implemented on `main`, packaged in a draft, and publicly available through the updater.
- Record clips when visible behavior works. Describe unreleased clips as development work; use the published version when saying a feature is available.

## Release checklist

1. **Choose the cutoff.** Select the exact commit and included changes. Everything in that snapshot ships; no routine cherry-picking or release branch is required.
2. **Write the notes.** Rewrite [`docs/release-notes.md`](release-notes.md): first line `# <version>`, then 3–5 one-line summary bullets, then `##` sections with short user-facing changes, fixes, platform limitations, and whether updating requires ending running shells. Do not paste a commit log or claim unfinished features.
3. **Set the version.** Update the root workspace version and lockfile consistently. Check any packaging/version references required by the existing scripts.
4. **Check the source.** Run the existing relevant formatting, lint, tests, and CI. Keep verification bounded to the changed behavior and existing release requirements; no new blanket qualification matrix.
5. **Commit and tag.** With explicit authorization, commit release preparation and tag that exact commit as `v<workspace-version>`. Push the tag to trigger the Release workflow. Never move an already published tag to different source.
6. **Check the draft.** Both platform jobs, package smoke checks, checksums, and metadata signing must succeed. Check the packaged app and the update from the previous published version where native hosts are available. State unavailable checks plainly; do not treat cross-compilation as native qualification.
7. **Publish deliberately.** Publish the completed draft as a stable release. Do not mark it prerelease if it is intended for the current stable updater.
8. **Verify discovery.** Read back the published release and assets. Confirm the previous installed version can discover the new version, show the intended signed notes, and follow the expected update path without touching unrelated running work.
9. **Post.** Show the shipped behavior with short clips/screenshots and point people to the available download. No need to wait for a large launch campaign.

A failed package/update check is a blocker to investigate, not a reason to invent success or publish incomplete assets. Do not retag published source as a workaround.

## Existing release and updater behavior

At inspected upstream `5aff8fa51a2f70be9cd2c1896f80b451aa4dac74`:

- A `v*` tag triggers the Release workflow. Build scripts check the expected tag against the workspace version.
- Windows and macOS packages are built and smoke-checked; the publish job depends on both jobs.
- Checksums are combined/verified and native release metadata is signed with `docs/release-notes.md` as its notes. The Windows job fails first if that file's heading does not match the tag.
- The workflow creates a draft GitHub release for deliberate publication.
- Manual workflow dispatch produces build artifacts, not a public release.
- The updater requests GitHub's latest release and rejects drafts/prereleases. Pushing commits or a tag alone does not make an update available.
- Update checks do not silently download, install, close windows, or restart daemons.

### Running shells

Client replacement and daemon restart are different operations.

- Compatible activation can relaunch the client while retaining its existing daemon and shells.
- A required local daemon restart needs explicit consent and ends its shells.
- Local updates do not restart remote daemons.
- Advertise daemon compatibility only when qualified. Do not promise uninterrupted work for every release or equate matching protocol numbers with a verified update path.

Publisher signing/notarization and signed updater metadata are separate concerns. Follow the existing distribution guide; do not imply production OS trust from local test signatures.

## Release notes

`docs/release-notes.md` is the one source. The client embeds it at build time and shows it only when its heading matches the client version: once as the **What's new** card in the sidebar after an update (an accent dot on the sidebar button until the sidebar is opened; the card until its × is clicked), and always under **What's new in <version>** in **Settings → Updates**. The workflow signs it into the update metadata, which the update dialog shows before installing, and uses it as the draft GitHub release body. Editing only the GitHub description afterward changes neither the app nor the updater, so write final notes before tagging.

## Latest release: v0.1.5

Fix release for the update blocker found right after v0.1.4: on Windows, closing the console window of the sign-in task's supervisor killed it while its daemon kept running, and the 0.1.3 updater then refused to inspect that daemon ("cannot match target supervisor lifecycle identity"). v0.1.5 detaches the supervisor from its console and treats an exited supervisor as an unsupervised daemon. Installs still on 0.1.3 run their own old check, so the notes give the one-time `--shutdown` command to unstick them.

## Previous release: v0.1.4

Published 2026-10-06 as the stable latest release from tag `v0.1.4` (`6f3a386`); notes from `docs/release-notes.md`.

- Floating terminals.
- Layout presets, arrangement preview/apply, mirror/flip, swap, and restore.
- Combining tabs and splitting them back out.
- Shell prompt settings (Oh My Posh/Starship) and the one-time What's new card.

Protocol 16 (v0.1.3 shipped 14), so updating from 0.1.3 restarts the daemon and ends running shells. The draft's publish step stopped silently after one upload on the first run (`softprops/action-gh-release@v2`, now forced onto Node 24); deleting the partial draft and rerunning the publish job succeeded. Read back: 10 assets, signed metadata with version 0.1.4, protocol 16, qualified daemon 0.1.4, and the full notes; the Windows update package hash matched. Not yet exercised: the in-app update from an installed 0.1.3, and the macOS app on a native Mac.

## References

- Repository: https://github.com/cloudboy-jh/compi
- Release workflow: `.github/workflows/release.yml`, especially tag triggers and lines 194–234 in the inspected revision.
- Distribution guide: `docs/DISTRIBUTION.md`.
- Stable discovery: `crates/compi-update/src/lib.rs:368–382`.
- Displayed signed notes: `crates/compi-client/src/updates.rs:803–806`.
- Native/source evidence: `docs/COMPLETED.md` and `docs/NEXT_STEPS.md`.
- [[feat and bugs]] · [[dev tricks]] · [[Test Cmds]]

This note records the agreed release approach and inspected implementation. It does not create a tag, publish a release, or claim a new native update test.
