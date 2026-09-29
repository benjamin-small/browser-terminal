# Releases

browser-terminal uses semantic versioning when it publishes versioned artifacts.
User-visible changes are summarized in release notes, including upgrade steps
and breaking changes.

## v0.5.0 — Browser filesystem by default

- Automatically mount writable OPFS at `/scratch` with filesystem commands,
  editor, redirection, path completion, and directory prompts. Missing or
  denied storage warns in both the terminal and console and retains the core shell.
- Hosts that install their own adapter can retain their setup with
  `BrowserTerminal.create({ filesystem: false })`. `bt.filesystem` exposes the
  automatic adapter when available.

- Use `to-json`, `from-json`, `str-upcase`, and `str-downcase` for the JSON and
  string commands. Spaced forms remain callable compatibility aliases; help
  and command-name completion advertise the hyphenated names.
- The demo adds an Enable writes button for each connected local folder, so
  shell redirects can create files without opening the editor first.

See the [release notes](release-notes/v0.5.0.md) for upgrade guidance and coverage.

## v0.4.0 — Browser files and editing

Adds optional browser filesystem mounts, navigation, bounded file reads and
writes, text editing, command and argument completion, and directory prompts.
The demo starts in browser-private `/scratch`; local folders connect under `/mnt`.

See the [release notes](release-notes/v0.4.0.md) for changes, upgrade guidance,
validation, and known browser limitations. The
[implementation plan](superpowers/plans/2026-09-28-browser-filesystem-v0.4.0.md)
records the design and scope.

## Release procedure

Run the documented validation commands, confirm generated artifacts are current,
and verify the packed artifact from a clean checkout. Run the release workflow
with `dry_run: true` before pushing a version tag. Record native folder save and
manual browser coverage explicitly in the release notes. Tags matching `v*`
trigger npm publishing through `.github/workflows/release.yml`; pushes to `main`
deploy the demos to GitHub Pages.
