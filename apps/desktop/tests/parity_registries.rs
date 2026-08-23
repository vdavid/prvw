//! Proof that the parity registries fail the build when a platform skips an entry.
//!
//! Layer 1 of the parity harness (`src/parity/`) rests on one claim: a platform's builder
//! consumes each registry through an exhaustive `match`, so a new setting, menu item, or
//! command can't be ignored anywhere. A claim about compilation can't be an ordinary `#[test]`,
//! so these two run `rustc` over the fixtures in `parity_fixtures/` and check that the one
//! missing an arm is rejected and the one answering every entry is accepted.
//!
//! Running `rustc` directly, rather than through a compile-fail harness like `trybuild`, is
//! what keeps this cheap: the fixtures load `src/parity/` with `#[path]` and the tree has no
//! dependencies, so each compile is one file and takes about a tenth of a second. It also
//! keeps a dependency out of the build for something this small. Cargo doesn't build files in
//! `tests/parity_fixtures/` as test targets, so they're compiled here and nowhere else.

use std::path::PathBuf;
use std::process::{Command, Output};

fn compile(fixture: &str) -> Output {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out_dir = tempfile::tempdir().expect("temp dir");
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    Command::new(rustc)
        .arg("--edition")
        .arg("2024")
        .arg("--crate-type")
        .arg("lib")
        .arg("--emit")
        .arg("metadata")
        .arg("--out-dir")
        .arg(out_dir.path())
        .arg(root.join("tests/parity_fixtures").join(fixture))
        .output()
        .expect("rustc runs")
}

#[test]
fn a_platform_that_misses_an_entry_fails_the_build() {
    let output = compile("misses_an_arm.rs");
    assert!(
        !output.status.success(),
        "a builder that skips registry entries compiled, so nothing is holding the platforms \
         together any more"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // E0004 is "non-exhaustive patterns". Every registry has to produce one: a platform can
    // forget a setting, a menu item, or a command, and all three have to be caught.
    assert_eq!(
        stderr.matches("error[E0004]").count(),
        3,
        "expected one non-exhaustive-match error per registry, got: {stderr}"
    );
    for registry in ["SettingKey", "MenuItemKey", "CommandKey"] {
        assert!(
            stderr.contains(registry),
            "{registry} didn't complain about its missing arms: {stderr}"
        );
    }
    // The error names the entries that were skipped, which is what makes it a to-do list
    // rather than a riddle.
    assert!(
        stderr.contains("not covered"),
        "the error should name the entries that were skipped: {stderr}"
    );
}

#[test]
fn a_platform_that_answers_every_entry_builds() {
    let output = compile("handles_every_arm.rs");
    assert!(
        output.status.success(),
        "the control fixture failed to compile, so the failure above proves nothing: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
