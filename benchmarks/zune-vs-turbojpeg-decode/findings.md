# Findings: JPEG decode benchmark

Date: 2026-04-11. Machine: MacBook Pro M-series. Rust stable 1.94.

## Test images

20 photos from a Google Pixel phone (12 MP, 4080x3072 or 3072x4080, 2-9 MB JPEG). Copied to local disk to isolate CPU
decode time from network I/O.

## Results (local disk, /tmp)

```
Image               Dimensions  File size         zune-jpeg    turbojpeg/full     turbojpeg/1:2     turbojpeg/1:4     turbojpeg/1:8
---------------------------------------------------------------------------------------------------------------------------------
1.jpg             3072x4080       5.5 MB        60.3+-0.2ms       44.5+-0.0ms       36.9+-0.1ms       34.5+-0.2ms       32.8+-0.0ms
10.jpg            3072x4080       4.9 MB        47.1+-0.3ms       34.1+-0.2ms       27.6+-0.4ms       24.0+-0.0ms       22.8+-0.1ms
11.jpg            4080x3072       5.3 MB        46.5+-0.1ms       34.3+-0.1ms       27.4+-0.1ms       24.3+-0.1ms       23.0+-0.1ms
12.jpg            3072x4080       4.8 MB        50.6+-0.1ms       37.4+-0.2ms       30.3+-0.1ms       27.2+-0.1ms       25.9+-0.1ms
13.jpg            4080x3072       5.4 MB        49.7+-0.3ms       36.7+-0.0ms       29.7+-0.1ms       26.6+-0.1ms       25.4+-0.0ms
14.jpg            2736x3648       7.3 MB        31.0+-0.2ms       22.9+-0.1ms       17.1+-0.3ms       14.7+-0.0ms       13.8+-0.0ms
15.jpg            4080x3072       6.7 MB        50.2+-0.1ms       36.9+-0.1ms       29.9+-0.0ms       26.7+-0.0ms       25.5+-0.0ms
16.jpg            3072x4080       3.1 MB        43.2+-0.1ms       32.4+-0.2ms       25.3+-0.0ms       22.2+-0.0ms       21.0+-0.0ms
17.jpg            4080x3072       5.4 MB        54.3+-0.2ms       39.7+-0.2ms       32.5+-0.1ms       29.4+-0.1ms       28.0+-0.1ms
18.jpg            4080x3072       9.1 MB        76.6+-0.1ms       54.4+-0.1ms       47.0+-0.1ms       44.5+-0.1ms       42.4+-0.0ms
19.jpg            4080x3072       7.1 MB        76.0+-0.0ms       53.8+-0.0ms       46.4+-0.0ms       43.9+-0.0ms       41.8+-0.1ms
2.jpg             4080x3072       5.8 MB        65.8+-0.2ms       47.1+-0.0ms       39.5+-0.0ms       37.3+-0.8ms       35.2+-0.0ms
20.jpg            4080x3072       5.4 MB        46.0+-0.3ms       34.0+-0.0ms       27.0+-0.0ms       23.9+-0.0ms       22.8+-0.0ms
3.jpg             3072x4080       3.6 MB        41.2+-0.3ms       30.4+-0.1ms       23.7+-0.1ms       20.5+-0.0ms       19.2+-0.0ms
4.jpg             4080x3072       2.1 MB        43.3+-0.1ms       32.6+-0.1ms       25.9+-0.1ms       22.6+-0.1ms       21.2+-0.1ms
5.jpg             3072x4080       3.3 MB        41.6+-0.1ms       31.2+-0.1ms       24.7+-0.3ms       21.1+-0.0ms       20.0+-0.0ms
6.jpg             4080x3072       6.5 MB        54.4+-0.2ms       39.9+-0.2ms       32.5+-0.0ms       29.6+-0.0ms       28.2+-0.0ms
7.jpg             3072x4080       5.9 MB        43.3+-0.2ms       32.1+-0.2ms       25.0+-0.1ms       21.9+-0.0ms       20.6+-0.1ms
8.jpg             3072x4080       6.9 MB        53.7+-1.3ms       38.4+-0.0ms       31.3+-0.1ms       28.2+-0.0ms       27.0+-0.0ms
9.jpg             4080x3072       4.2 MB        52.2+-0.1ms       38.5+-0.2ms       31.0+-0.0ms       28.3+-0.1ms       26.9+-0.0ms
---------------------------------------------------------------------------------------------------------------------------------
Averages                                        51.3+-11.0ms      37.6+-7.5ms       30.5+-7.2ms       27.6+-7.4ms       26.2+-7.1ms
```

## Results (NAS, SMB mount over gigabit ethernet)

Same images, same benchmark binary, but read from `/Volumes/naspi-1/tmp/prvw-bench/`. The benchmark reads files into
memory before timing, so these numbers isolate CPU decode (not I/O).

```
Averages                                        51.1+-11.0ms      37.6+-7.4ms       30.6+-7.3ms       27.6+-7.3ms       26.2+-7.1ms
```

Identical to local. Confirms the benchmark measures pure decode, not I/O.

## Summary

| Scenario | Average decode | vs zune-jpeg | vs turbojpeg full |
|---|---|---|---|
| zune-jpeg (pure Rust) | 51.3ms | baseline | +36% |
| turbojpeg full (C) | 37.6ms | -27% | baseline |
| turbojpeg 1/2 | 30.5ms | -41% | -19% |
| turbojpeg 1/4 | 27.6ms | -46% | -27% |
| turbojpeg 1/8 | 26.2ms | -49% | -30% |

## Key insights

1. **turbojpeg is ~27% faster** than zune-jpeg for full-resolution decode on Apple Silicon.
2. **DCT scaling helps less than expected**: 1/8 is only ~30% faster than full turbojpeg. Most time is in
   Huffman/DCT, not pixel output.
3. **At 50ms per image, decode is not the bottleneck.** The 2-3 second delays observed in the Prvw app when browsing
   NAS photos were caused by network I/O (SMB file reads), not CPU decode.
4. **Parallel preloading is the highest-impact optimization**: reading/decoding 4 images simultaneously from NAS
   cuts worst-case latency from sequential sum (~10s) to the single slowest image (~2.5s).

## Decision

Use **zune-jpeg** (pure Rust, simpler build, no C dependency). The 27% speed difference is ~13ms per image, which is
imperceptible in practice. The real bottleneck (NAS I/O) is addressed by parallel preloading with rayon, not by a
faster decoder.
