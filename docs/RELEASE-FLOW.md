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
2. **Write the notes.** Short user-facing changes, fixes, platform limitations, and whether updating requires ending running shells. Do not paste a commit log or claim unfinished features.
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
- Checksums are combined/verified and native release metadata is signed.
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

## Release notes: proposed small improvement

**Proposal, not implemented by this note:** keep one versioned Markdown notes file in the repository and feed its text into both the signed update manifest and GitHub release body.

Current workflow generates GitHub notes before signing the update metadata. The app displays the notes embedded in the signed manifest. Editing only the GitHub description afterward does not change the updater's notes.

Write final notes before generating/signing packages. If the source-of-notes workflow is changed, verify both outputs contain the intended text. No separate changelog service or release-management system is needed.

## Proposed next batch: v0.1.4

Proposal only; no release has been created by this note.

- Floating terminals.
- Layout presets, arrangement preview/apply, mirror/flip, swap, and restore.
- Combining tabs and splitting them back out.
- Related workspace polish actually present at the chosen cutoff.
- Developer-note mention of `cargo dev`, which is not packaged as an end-user feature.

Inspected `v0.1.3` points to `43c0b576a48d7009028146dd94e2c5c0670d9a92`. Floating panes, arrangement implementation, and `cargo dev` are absent from that tag and present on inspected `main`. Recheck the latest release before preparing this batch; this is not a permanently fixed next-version instruction.

## References

- Repository: https://github.com/cloudboy-jh/compi
- Release workflow: `.github/workflows/release.yml`, especially tag triggers and lines 194–234 in the inspected revision.
- Distribution guide: `docs/DISTRIBUTION.md`.
- Stable discovery: `crates/compi-update/src/lib.rs:368–382`.
- Displayed signed notes: `crates/compi-client/src/updates.rs:803–806`.
- Native/source evidence: `docs/COMPLETED.md` and `docs/NEXT_STEPS.md`.
- [[feat and bugs]] · [[dev tricks]] · [[Test Cmds]]

This note records the agreed release approach and inspected implementation. It does not create a tag, publish a release, change the workflow, or claim a new native update test.
