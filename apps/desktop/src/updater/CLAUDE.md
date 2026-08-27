# Update check

Whether a newer Prvw exists, and what each platform does about it. `updater.rs` beside this directory is the module root
and carries the map; the `//!` docs on each file carry the rest. Read those first: this file only holds the decisions
and the traps.

## Decision: the check is pure, the acting is fenced

**Why:** the settings toggle is one setting on every platform, and it has to mean the same thing everywhere or it lies.
`manifest.rs` holds all of it (the manifest shape, the key each build looks for, the semver comparison, the
once-per-release rule) with nothing platform-specific in it, so a Mac asserts what a Windows build will decide.
`macos.rs` and `windows.rs` hold only what touches the machine. Same shape as `chrome.rs` and `paths.rs`.

## Decision: Windows opens the browser instead of putting up a window

**Why:** three reasons, in order.

1. **A window of ours would need a message loop**, and a nested one starves winit's pump: the slideshow timer stops and
   `about_to_wait` stops running (the gotcha in `AGENTS.md`). The modeless `CreateDialogParamW` plus
   `platform::windows::msg_hook` is the shape that works, and it's a lot of surface for one sentence a person reads once
   a release.
2. **The browser is where the update actually happens.** Windows ships as an NSIS installer that wants a person to click
   through it, so there's no self-update to offer. Every path from "there's a new version" ends at a download either
   way.
3. **A viewer earns no notification chrome.** Prvw opens, shows a picture, and closes.

## Decision: a version is announced once, tracked by a file

**Why:** without it, someone who looks at the new release and decides to stay where they are gets a browser tab on every
single launch, and a viewer gets launched dozens of times a day. `update-announced` in the app data directory holds the
last version we opened a page for. It's a plain file rather than a `Settings` field because nothing about it is a
preference: losing it costs one extra tab, which isn't worth a schema, a parity entry, or a row in the QA server's
state.

## Gotcha: only an installed copy checks, and each platform means something different by it

macOS won't check unless it's running from `/Applications`, because it swaps its own bundle and has no business doing
that to a copy in `~/Downloads`. Windows only ever opens a browser, so a portable copy checking is fine; what it keeps
out is the build tree, via `debug_assertions`. That's also what stops the E2E harness opening a browser mid-run, since
`cargo test` builds debug.

## Gotcha: Windows asks for a different TLS backend, and it isn't a preference

`reqwest`'s `rustls` feature means `aws-lc-rs`, which wants a NASM assembler that neither this Mac nor `cargo xwin` has,
so a Windows target that asks for it stops cross-compiling. The Windows entry in `Cargo.toml` asks for `native-tls`
instead, which binds Schannel through the pure-Rust `schannel` crate and needs no C toolchain. Don't unify the two
features.

## Gotcha: Linux compiles the policy and calls none of it

Prvw publishes no Linux builds, so there's nothing to check against and nowhere to send anyone. `manifest.rs` still
compiles and tests there (that's the point of it being pure), which is why `main.rs` carries an `allow(dead_code)` for
the platforms with no acting half.
