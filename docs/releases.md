# Releases

browser-terminal uses semantic versioning when it publishes versioned artifacts. User-visible changes should be summarized in release notes, including upgrade steps and breaking changes.

Before a release, run the repository's documented validation commands, confirm generated artifacts are current, and verify the release from a clean checkout.

## Next release: v0.4.0 (release candidate)

The implementation adds optional browser filesystem mounts, navigation, file
reads and writes, and text editing, with experimental file-backed block access
and virtual devices. The candidate is implemented but not published.

The [implementation plan](superpowers/plans/2026-09-28-browser-filesystem-v0.4.0.md)
defines behavior, work packages, tests, and release gates. The
[draft release notes](release-notes/v0.4.0-draft.md) track the intended user-facing
changes. Update them to match completed work before publishing.

Cargo and npm manifests and their lockfiles now target 0.4.0. Verify the exact
packed artifact from a clean checkout and run the release workflow with
`dry_run: true` before pushing a version tag. Record native folder save and manual browser coverage explicitly in the release
notes; v0.4.0 ships with the remaining coverage limitations documented. Tags matching `v*` trigger
npm publishing through `.github/workflows/release.yml`.
