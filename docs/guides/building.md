# Building Prvw

## Prerequisites

- Rust stable (managed by `rust-toolchain.toml` at repo root)
- macOS with Metal support (for wgpu)
- For signing: Apple Developer ID certificate for "Rymdskottkarra AB (83H6YAQMNP)" in your Keychain

## Dev build

```sh
cd apps/desktop
cargo build
cargo run -- /path/to/image.jpg
```

Use `RUST_LOG=debug` for verbose logging, or target specific modules:

```sh
RUST_LOG=prvw::renderer=debug cargo run -- /path/to/image.jpg
```

## Release build with code signing

```sh
./scripts/build-and-sign.sh
```

This builds a release binary, signs it with hardened runtime using the Developer ID certificate, and verifies the signature. The signed binary ends up at `apps/desktop/target/release/prvw`.

## Running checks

```sh
# All checks
./scripts/check.sh

# Specific checks
./scripts/check.sh --check clippy
./scripts/check.sh --check rustfmt
./scripts/check.sh --check cargo-test
```

## Tests

```sh
cd apps/desktop
cargo test
```

GPU-dependent tests (renderer) are marked `#[ignore]` since they need a real GPU. Run them locally with:

```sh
cargo test -- --ignored
```
