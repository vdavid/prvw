# Changelog format

`CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) with one strict house rule on top:
**every entry is short and links its commit(s)**.

Validated by the `changelog-commit-links` check (`./scripts/check.sh --check changelog-links`, ~50 ms). The check runs
in CI on any PR / push that touches `CHANGELOG.md`.

## Entry rules

- **One line by default.** Two lines if the "why" needs context. Three lines only for genuinely big shifts (an
  end-to-end rework, a new subsystem, a tricky cross-cutting fix). Long-form prose goes in `docs/notes/` and is linked
  from the entry.
- **Lead with the impact**, not the mechanism. `**Navigation no longer freezes on slow shares.**` beats
  `Moved QuickLook submission off the main thread`. The mechanism follows in the same sentence.
- **Bold the lead** for entries that sit at the top of an `### Added` / `### Changed` / `### Fixed` block or are
  obviously the headline change. Plain text for the smaller follow-ups.
- **End with the commit link(s)** in the form `([8charsha](https://github.com/vdavid/prvw/commit/8charsha))`. Use the
  full 8-character prefix from `git log --abbrev=8`. If multiple commits ship the same change, list them all
  comma-separated.
- **Skip pure docs / chore commits** (CLAUDE.md syncs, em-dash cleanups, formatting, .gitignore tweaks). They live in
  git, not the changelog.

Use the existing entries in `CHANGELOG.md` as the reference — they all match this shape.

## Section order

Per Keep a Changelog: `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`. Skip the ones that don't apply.

## The `[Unreleased]` section

Always sits directly after the format preamble, before the latest versioned section. Add entries here as you ship work;
the release script (`scripts/release.sh`) replaces `## [Unreleased]` with `## [x.y.z] - YYYY-MM-DD` on tag, then the
next change re-creates the `[Unreleased]` heading. Don't pre-create empty subsections — the release script's regex
expects a populated block.

## What the check enforces

- Every `https://github.com/vdavid/prvw/commit/<sha>` URL resolves to a real commit reachable from `HEAD`.
- Any `[sha](url)` paired link has matching SHAs (catches typos where the visible text drifts from the URL).
- SHAs are 6-40 hex chars (catches truncations and overflows).

CI runs the check with `fetch-depth: 0` so reachability sees the full history, not just the latest commit. A SHA that
resolves locally via reflog but isn't merged into `HEAD` (a rebased-away commit) fails CI — exactly the case where a
local `git show <sha>` would mislead you.

## Tips for writing entries

- Read the existing nearby entry before adding yours; pattern-match the tone.
- If you're tempted to write a paragraph, that's a signal the entry belongs in `docs/notes/<feature>.md` with a one-line
  pointer from `CHANGELOG.md`.
- Pull the SHA after the commit lands. `git log --abbrev=8 -1 --pretty=format:%h` gives you the right form.
- For entries that span several commits (a multi-step feature, a perf pass), list them in chronological order.
