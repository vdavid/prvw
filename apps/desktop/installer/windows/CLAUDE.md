# The Windows installer

`PrvwSetup-<version>-x64.exe`, built by `scripts/build-windows-installer.sh`. NSIS, compiled by `makensis`, which runs
natively on macOS, so the installer comes off the same Mac as the exe it wraps. A release builds it on `windows-latest`
instead, running the same script with `--exe` over a natively compiled binary.

| File                    | Purpose                                                                 |
| ----------------------- | ----------------------------------------------------------------------- |
| `prvw.nsi`              | The whole installer and uninstaller. UTF-8 **with a BOM**               |
| `file-associations.nsh` | Generated: the registry writes and their inverse. Never edit it by hand |

- Build it: `./scripts/build-windows-installer.sh` (see [releasing.md](../../../../docs/guides/releasing.md)).
- Keep it honest: `./scripts/check.sh --check installer`, which needs no makensis and answers on a Mac.

## Decision: NSIS, because it's the only candidate that cross-builds

**Decision:** NSIS. Not WiX, Inno Setup, or MSIX.

**Why:** the whole Windows build story here is `cargo-xwin` on a Mac, and an installer toolchain that only runs on
Windows would end that. `makensis` is the one that doesn't: it compiles on POSIX hosts and ships the Windows stubs and
plugins prebuilt, so a Mac emits a real PE32 installer. WiX v4+ is a .NET tool but still needs Windows components
outside .NET, and the usual Linux recipe is WiX under Wine. Inno Setup's compiler is Windows-only. MSIX needs the
Windows SDK to pack, and its sandbox would complicate both the updater and the QA server's localhost port (see M7 in
`docs/specs/cross-platform-plan.md`). NSIS is zlib-licensed, so nothing about it constrains what Prvw can be.

The cost is that NSIS scripting is its own small language with sharp edges, which is why the two that bite are written
down below.

## Decision: per-user install, so there's no UAC prompt

**Decision:** `RequestExecutionLevel user`, into `$LOCALAPPDATA\Programs\Prvw`, with the uninstall entry under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Prvw`.

**Why:** three things line up behind it. The file-type registration is HKCU-only anyway, because Windows gives an app no
machine-wide say in defaults since 10 20H2 (`../../src/settings/windows/file_types.rs`), so a machine-wide install would
buy nothing there. The updater can replace files without asking anyone. And an image viewer that opens with a UAC prompt
is one people close again. It's the same shape VS Code and GitHub Desktop ship.

What it costs: Prvw installs for one user rather than everyone on the machine. Worth revisiting only if someone asks for
a deployable build, which would be a separate MSI rather than a flag on this one.

## Decision: the registry writes are generated, not written twice

**Decision:** `file-associations.nsh` is rendered by `cargo xtask installer-registry` from
`settings::windows::file_types`, and the `installer` check fails when the committed file and that module disagree.

**Why:** the app's "Register Prvw's file types" button and the installer have to write the same keys, or a repair from
Settings would quietly disagree with what the install did. NSIS can't link a Rust crate to ask, so the alternative was a
second list that drifts the first time a format is added. Adding an extension to `decoding::dispatch` now reaches the
installer with no further thought.

The uninstaller's side comes from `file_types::removal`, which is the exact inverse and is tested as such.

## Gotcha: the .nsi is UTF-8 with a BOM, and has to stay that way

**Gotcha:** makensis reads a `.nsi` in the build host's ANSI code page unless a BOM says otherwise. Without it,
"Rymdskottkärra AB" in the installer's version info becomes whatever the build machine's locale makes of those bytes,
and nothing fails: you get a shipped installer with a mangled publisher. The build script also passes
`-INPUTCHARSET UTF8`, and the `installer` check asserts the BOM, because either one alone is a single point of failure.

## Gotcha: every path makensis is handed has to be in the host's own notation

**Gotcha:** makensis splits a path on the host separator and only that one, `\` on Windows and `/` everywhere else.
(`get_dir_name` in the NSIS source says so, with a `BUGBUG` beside it.) A path glued together out of `${__FILEDIR__}`
and a `/` therefore carries both separators on a Windows host, and `!include` resolves through a directory listing, so
it reads everything after the last `\` as the file name and goes looking for `windows/file-associations.nsh` inside the
parent folder. Only `!include` breaks: `Icon` and the licence page reach their file through `fopen`, which takes either
separator, so three of the four paths worked and hid the fourth until a release run.

So `prvw.nsi` builds no path of its own. `scripts/build-windows-installer.sh` passes `PRVW_INSTALLER_DIR`,
`PRVW_LICENSE`, and `PRVW_ICON` beside `PRVW_EXE` and `PRVW_OUTFILE`, every one of them through the same `cygpath`
helper, and the include is a bare file name against `!addincludedir`, so makensis joins the directory to the name
itself. Keep it that way: a `${...}/...` anywhere in this file is the bug coming back. The `${__FILEDIR__}` defaults
exist only for running makensis by hand with no script around it.

## Gotcha: `$` is NSIS's escape character, in every kind of quote

**Gotcha:** `$` introduces a variable in `"..."`, `'...'`, and backticks alike; a literal one is `$$`. That's why the
generator renders the executable's path as a placeholder and only substitutes `$INSTDIR\prvw.exe` after escaping
(`xtask/src/installer_registry.rs`). Quotes inside a string are handled by picking a delimiter the value doesn't
contain, which NSIS allows and which reads better than `$\"`.

## Gotcha: a running Prvw is detected by opening its own exe

`EnsurePrvwIsClosed` opens `$INSTDIR\prvw.exe` for append and treats the failure as "it's running". Windows won't let
anyone open a running executable for writing, and the bundled plugin set has nothing that enumerates processes, so this
is the check that needs no extra dependency. In a silent run (`/S`) there's nobody to close the window, so it aborts
rather than hanging.

## Where signing goes

Nothing here signs anything. `scripts/build-windows-installer.sh` runs `$PRVW_WINDOWS_SIGN_CMD <file>` if that variable
is set, once for `prvw.exe` before packaging and once for the finished installer. The release workflow passes it through
from a repository variable of the same name, which nobody has set, so releases ship unsigned until Azure Trusted Signing
supplies a command. The uninstaller NSIS extracts at install time stays unsigned; signing it needs the two-pass build
described in `docs/guides/releasing.md`.

## What needs a real Windows box

Everything the installer does at run time. Nothing below has executed:

- Whether the wizard's pages look right at 100%, 150%, and 200% scaling.
- Whether the Start-menu shortcut, the Apps & features entry, and its size are what the wizard promised.
- Whether Explorer's "Open with" list shows Prvw straight after the install, or wants a sign-out despite
  `SHChangeNotify`.
- Whether Prvw gets its own page under Settings → Apps → Default apps, which is what the `Capabilities` block is for.
- Whether `${IsNativeARM64}` reads true on Windows on ARM, so the x64 build installs there under emulation.
- Whether an install over a running Prvw shows the retry message rather than failing on a locked file.
- Whether the uninstall leaves nothing behind in `HKCU\Software\Classes`.
