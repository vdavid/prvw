//! Repo tasks that read the desktop app's registries without building the app.
//!
//! Two tasks, each printing a generated file to stdout for the check runner to compare against
//! the committed one, so there is exactly one writer:
//!
//! - `parity` renders `docs/parity.md` (`./scripts/check.sh --check parity`).
//! - `installer-registry` renders the Windows installer's file-type registration
//!   (`./scripts/check.sh --check installer`).
//!
//! Both trees are loaded with `#[path]` rather than through a dependency, the same way
//! `apps/desktop/tests/parity_fixtures/` loads the parity registries. They carry no dependencies
//! and no `#[cfg]`, so this crate compiles in well under a second on any host and answers for
//! every platform. Building the app instead would mean wgpu, rawler, and a GPU-shaped dependency
//! graph for a task that prints text.

// The app consumes the whole tree; this tool only reads `report()`.
#[allow(dead_code)]
#[path = "../../apps/desktop/src/parity/mod.rs"]
mod parity;

// What the installer's registry include is rendered from. The decoder's extension tables come in
// on their own, because `#[path]` on an inline module resolves against a directory named after
// that module, and only a file at the crate root can spell the app's path the way `parity` does.
#[allow(dead_code)]
#[path = "../../apps/desktop/src/decoding/dispatch.rs"]
mod dispatch;

// `file_types` asks for its extension list through `crate::decoding::supported_extensions`, so
// this stands in for the app's `decoding` module and forwards to the tables above.
mod decoding {
    /// Every extension the app opens, straight from the decoder's own tables.
    pub fn supported_extensions() -> Vec<&'static str> {
        crate::dispatch::supported_extensions()
    }
}

#[allow(dead_code)]
#[path = "../../apps/desktop/src/settings/windows/file_types.rs"]
mod file_types;

mod installer_registry;
mod parity_table;

use std::process::ExitCode;

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("parity") => {
            print!("{}", parity_table::render(&parity::report()));
            ExitCode::SUCCESS
        }
        Some("installer-registry") => {
            print!("{}", installer_registry::render());
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("xtask: unknown task {other:?}");
            usage();
            ExitCode::FAILURE
        }
        None => {
            eprintln!("xtask: no task given");
            usage();
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!("usage: cargo xtask <task>");
    eprintln!();
    eprintln!("tasks:");
    eprintln!("  parity              Print the platform parity table (docs/parity.md) to stdout.");
    eprintln!("                      To update the file, run `./scripts/check.sh --check parity`.");
    eprintln!(
        "  installer-registry  Print the Windows installer's file-type registration to stdout."
    );
    eprintln!(
        "                      To update the file, run `./scripts/check.sh --check installer`."
    );
}
