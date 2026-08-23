# Building Prvw

## Prerequisites

- Rust stable (managed by `rust-toolchain.toml` at repo root)
- macOS with Metal support (for wgpu), to run the app
- For signing: Apple Developer ID certificate for "Rymdskottkarra AB (83H6YAQMNP)" in your Keychain

Windows and Linux compile the desktop crate and pass its unit tests, and CI enforces both, but neither is a working
viewer yet. See [../specs/cross-platform-plan.md](../specs/cross-platform-plan.md).

## Dev build

```sh
cd apps/desktop
cargo build
cargo run -- /path/to/image.jpg
```

Use `RUST_LOG=debug` for verbose logging, or target specific modules:

```sh
RUST_LOG=prvw::render::renderer=debug cargo run -- /path/to/image.jpg
```

## Release build with code signing

```sh
./scripts/build-and-sign.sh
```

This builds a release binary, signs it with hardened runtime using the Developer ID certificate, and verifies the
signature. The signed binary ends up at `apps/desktop/target/release/prvw`.

## Running checks

```sh
# All checks
./scripts/check.sh

# Specific checks
./scripts/check.sh --check clippy
./scripts/check.sh --check rustfmt
./scripts/check.sh --check cargo-test
```

On Windows, `scripts/check.ps1` is the entry point. Same flags, same exit code, since both wrap the same Go runner:

```powershell
.\scripts\check.ps1 --check clippy
```

To lint the Windows build from a Mac, without a Windows machine: `./scripts/check.sh --check windows-cross`. It's marked
slow, so a plain run leaves it out; setup steps are in [AGENTS.md](../../AGENTS.md).

## Tests

```sh
cd apps/desktop
cargo test
```

GPU-dependent tests (renderer) are marked `#[ignore]` since they need a real GPU. Run them locally with:

```sh
cargo test -- --ignored
```
