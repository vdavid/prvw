//! Repo tasks that read the desktop app's registries without building the app.
//!
//! One task today: `parity`, which prints `docs/parity.md` to stdout. The check runner owns the
//! file itself (`scripts/check/checks/parity-table.go`), so there is exactly one writer, and
//! `./scripts/check.sh --check parity` is what regenerates the doc.
//!
//! The parity tree is loaded with `#[path]` rather than through a dependency, the same way
//! `apps/desktop/tests/parity_fixtures/` loads it. It carries no dependencies and no `#[cfg]`,
//! so this crate compiles in well under a second on any host and answers for every platform.
//! Building the app instead would mean wgpu, rawler, and a GPU-shaped dependency graph for a
//! task that prints text.

// The app consumes the whole tree; this tool only reads `report()`.
#[allow(dead_code)]
#[path = "../../apps/desktop/src/parity/mod.rs"]
mod parity;

mod parity_table;

use std::process::ExitCode;

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("parity") => {
            print!("{}", parity_table::render(&parity::report()));
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
    eprintln!("  parity    Print the platform parity table (docs/parity.md) to stdout.");
    eprintln!("            To update the file, run `./scripts/check.sh --check parity`.");
}
