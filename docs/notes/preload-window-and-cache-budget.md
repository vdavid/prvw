# Preload window and image cache budget

Why `navigation::preloader::preload_count()` is derived from the cache budget instead of being a constant, and how the
floor was picked. Referenced from `src/navigation/CLAUDE.md`.

## The problem

M0 made the SDR cache budget RAM-proportional (`clamp(RAM / 64, floor, 512 MB)`) but left the preload window at a fixed
`±2`. Those two numbers are not independent: a window of `n` asks the cache to hold `2n + 1` decoded images at once. The
old fixed 512 MB budget was sized for exactly that, so scaling the budget down without scaling the window down means the
preloader fetches more than the cache retains.

That is worse than a narrow window, not equivalent to one. `ImageCache::insert` evicts LRU until the new entry fits, so
each preload evicts the previous one, and once the image on screen becomes the LRU entry its own neighbors evict it.
Every arrow key then pays for a fresh decode — the exact latency the preloader exists to remove, plus the CPU of
decoding images that are thrown away before anyone looks at them.

`previews::generation_radius` already had the answer: derive the radius from the budget, "so we never generate more than
we'll retain".

## The unit: `LARGE_DECODE_BYTES`

The cache charges exact `width * height * bytes_per_pixel`, so this constant is a **sizing unit for the window**, never
a per-entry charge. Its job is to keep the window from overrunning on the images that _can_ overrun it, so it is the
large end of what Prvw opens, not the median.

- Was 20 MP (`docs/notes/raw-support-phase5.md`'s reference), which is below what a current full-frame body shoots.
- Now 24 MP RGBA8 = 96,000,000 bytes. 24 MP is already this repo's reference large image: `color::transform`'s
  measurements use it, and so does the cross-platform plan's `profiles_match` note.
- Spot check of what real files look like, from the Photos library on the dev machine: 8.0, 10.6, 12.2, and 12.2 MP.
  Phone photos are roughly a third of the unit and simply fit more of themselves into the same bytes. Sizing the window
  for them would put every RAW shooter back in the thrashing case.
- RGBA8 rather than RGBA16F even though HDR RAW decodes are 8 bytes per pixel: `hdr_memory_budget()` doubles alongside,
  so the window comes out identical in both modes and only one constant is needed.

## The floor

`MIN_SDR_MEMORY_BUDGET = 3 × LARGE_DECODE_BYTES` (288 MB). Picked from the window, not the other way round: three
decodes is the image on screen plus one neighbor on each side, the narrowest window that still keeps navigation instant
whichever way the user turns.

The rejected alternative was `5 × LARGE_DECODE_BYTES` (480 MB), which would preserve `±2` everywhere. It makes the
scaling almost entirely inert: `RAM / 64` only lands between 480 and 512 MB for RAM between roughly 30.7 and 32.8 GB, so
every machine outside that sliver gets the old fixed budget back. That throws away the point of the RAM scaling, which
is that a 1 GB HDR cache plus GPU-side copies is a different proposition on an 8 GB Windows laptop with no unified
memory than on a 32 GB Mac.

At 288 MB the scaling is live from about 18.4 GB upward, which covers the 24 GB and 32 GB configurations.

## Measured outcome

Images assumed to be 24 MP (the sizing unit). "fetches" is `2n + 1`; "retains" is `budget / 96 MB`.

| RAM   | version           | budget | window | fetches | retains | result             |
| ----- | ----------------- | ------ | ------ | ------- | ------- | ------------------ |
| 8 GB  | pre-M0            | 537 MB | ±2     | 5       | 5       | ok                 |
| 8 GB  | M0 as merged      | 160 MB | ±2     | 5       | 1       | evicts 4 of 5      |
| 8 GB  | now               | 288 MB | ±1     | 3       | 3       | ok                 |
| 16 GB | pre-M0            | 537 MB | ±2     | 5       | 5       | ok                 |
| 16 GB | M0 as merged      | 268 MB | ±2     | 5       | 2       | evicts 3 of 5      |
| 16 GB | now               | 288 MB | ±1     | 3       | 3       | ok                 |
| 24 GB | pre-M0            | 537 MB | ±2     | 5       | 5       | ok                 |
| 24 GB | M0 as merged      | 403 MB | ±2     | 5       | 4       | evicts 1 of 5      |
| 24 GB | now               | 403 MB | ±1     | 3       | 4       | ok, one slot spare |
| 32 GB | pre-M0 / M0 / now | 537 MB | ±2     | 5       | 5       | unchanged          |
| 64 GB | pre-M0 / M0 / now | 537 MB | ±2     | 5       | 5       | unchanged          |

## The tradeoff, stated plainly

Against M0 as merged this is a strict improvement everywhere: no configuration thrashes any more.

Against pre-M0, an 8, 16, or 24 GB machine viewing 24 MP images gets a narrower window: `±1` instead of `±2`. Arrowing
two images at once is a cache miss where it used to be a hit. In exchange the image cache commits 288 MB instead of 537
MB (and 576 MB instead of 1 GB in HDR mode), which is the RAM-proportional behavior M0 step 5 asked for. A machine that
could afford `±2` was never going to be an 8 GB one; a 16 GB one is a real judgment call, and this is where it landed.
32 GB and up are untouched.

Folders of phone-sized photos are unaffected in practice: the cache charges their real size, so a 288 MB budget holds
six 12 MP decodes even though the window only asks for three.

## Knock-on

`browser::browse_warm_indices` warms the browse selection into this same `ImageCache`, and used to carry its own
`BROWSE_WARM_RADIUS = 2` with a comment saying it matched the preloader. It now takes the radius as a parameter and the
caller passes `preload_count()`, which deletes the duplicated constant and keeps the function pure.

`previews::scheduler::PRELOAD_HALF` deliberately stays at the maximum. It is only a priority boundary for preview
generation, costs no memory, and on the machines where the decode window narrows, having the preview ready matters more.
