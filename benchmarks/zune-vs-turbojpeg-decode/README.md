# JPEG decode benchmark: zune-jpeg vs turbojpeg

Compares decode performance of `zune-jpeg` (pure Rust, SIMD) vs `turbojpeg` (libjpeg-turbo C wrapper) on real-world
photos, including turbojpeg's DCT scaling at 1/2, 1/4, and 1/8 resolution.

## Running

```bash
# Build
cargo build --release

# Run on specific images
cargo run --release -- /path/to/photos/*.jpg

# Run on test images (if previously copied to /tmp)
cargo run --release -- /tmp/prvw-bench-images/*.jpg
```

The benchmark:
- Reads all files into memory first (benchmarks pure decode, not I/O)
- Runs each (image, scenario) combination 3 times
- Randomizes execution order to avoid cache bias
- Reports mean and standard deviation per image per scenario

## Scenarios

| Scenario | Library | Output resolution |
|---|---|---|
| zune-jpeg | zune-jpeg 0.5 (pure Rust, NEON/AVX SIMD) | Full |
| turbojpeg/full | libjpeg-turbo 1.4 (C, hand-tuned asm) | Full |
| turbojpeg/1:2 | libjpeg-turbo with DCT scaling | Half |
| turbojpeg/1:4 | libjpeg-turbo with DCT scaling | Quarter |
| turbojpeg/1:8 | libjpeg-turbo with DCT scaling | Eighth |

See [findings.md](findings.md) for results.
