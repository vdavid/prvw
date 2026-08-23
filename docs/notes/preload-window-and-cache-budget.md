# Preload window and image cache budget

Why `navigation::preloader::preload_count()` is derived from the cache budget instead of being a constant, and how the
floor, the divisor, and the sizing unit were each picked. Referenced from `src/navigation/CLAUDE.md`.

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

## The divisor

`SDR_RAM_DIVISOR = 32`, because **16 GB / 32 is exactly the 512 MB ceiling**. The budget this replaced was a flat 512
MB, so it was implicitly tuned for a 16 GB machine all along; the divisor just says so out loud. Two consequences worth
stating:

- The scaling can only ever shrink a machine _smaller_ than the one the flat budget assumed. 16 GB and up are untouched,
  which matters because that's the most common Mac configuration and the one Prvw is developed on.
- The case the scaling exists for is the one M0 step 5's sub-bullet actually raised: an 8 GB Windows laptop with no
  unified memory, paying for the GPU-side copies separately. It never asked for a regression at 16 GB.

`sixteen_gb_lands_exactly_on_the_ceiling` fails if the divisor and the ceiling ever drift apart, so the relationship is
enforced rather than commented.

Between the floor (about 8.6 GB) and the ceiling (16 GB) the budget varies but the window doesn't — everything in that
band is `±1`. That's honest rather than wasteful: the budget is the memory bound, the window is a coarse integer derived
from it, and the spare bytes do get used, because the cache charges each entry its real size.

**Deliberately four times more generous than `previews::budget_for_ram`, which takes 1/128.** Two reasons, both about
what a byte buys there versus here. A preview is ~4 MB against a decode's ~96 MB, so previews' 64 MB floor already holds
16 of them where ours has to clear three of a single unit. And previews buy grid smoothness across a ±50 window and
degrade to placeholders when short, while full decodes buy this image and the ones either side of it, where running
short is a visible decode on an arrow key.

## Measured outcome

Images assumed to be 24 MP (the sizing unit). "fetches" is `2n + 1`; "retains" is `budget / 96 MB`.

| RAM   | version           | budget | window | fetches | retains | result             |
| ----- | ----------------- | ------ | ------ | ------- | ------- | ------------------ |
| 8 GB  | pre-M0            | 537 MB | ±2     | 5       | 5       | ok                 |
| 8 GB  | M0 as merged      | 160 MB | ±2     | 5       | 1       | evicts 4 of 5      |
| 8 GB  | now               | 288 MB | ±1     | 3       | 3       | ok                 |
| 12 GB | pre-M0            | 537 MB | ±2     | 5       | 5       | ok                 |
| 12 GB | M0 as merged      | 201 MB | ±2     | 5       | 2       | evicts 3 of 5      |
| 12 GB | now               | 403 MB | ±1     | 3       | 4       | ok, one slot spare |
| 16 GB | pre-M0            | 537 MB | ±2     | 5       | 5       | ok                 |
| 16 GB | M0 as merged      | 268 MB | ±2     | 5       | 2       | evicts 3 of 5      |
| 16 GB | now               | 537 MB | ±2     | 5       | 5       | unchanged          |
| 24 GB | pre-M0            | 537 MB | ±2     | 5       | 5       | ok                 |
| 24 GB | M0 as merged      | 403 MB | ±2     | 5       | 4       | evicts 1 of 5      |
| 24 GB | now               | 537 MB | ±2     | 5       | 5       | unchanged          |
| 32 GB | pre-M0 / M0 / now | 537 MB | ±2     | 5       | 5       | unchanged          |
| 64 GB | pre-M0 / M0 / now | 537 MB | ±2     | 5       | 5       | unchanged          |

## The tradeoff, stated plainly

Against M0 as merged this is a strict improvement everywhere: no configuration thrashes any more.

Against pre-M0, only machines below 16 GB change, and they change in the direction the scaling was for. An 8 GB machine
commits 288 MB instead of 537 MB and gets `±1` instead of `±2`, so arrowing two images at once is a cache miss where it
used to be a hit. A 12 GB machine keeps 403 MB and the same `±1`. 16 GB and up are byte-for-byte what they were, window
included.

Folders of phone-sized photos are unaffected in practice: the cache charges their real size, so a 288 MB budget holds
six 12 MP decodes even though the window only asks for three.

## Rejected alternatives

Kept because each one is a reasonable-looking idea that someone will have again.

- **M0 as merged: scale the budget, leave the window at a fixed `±2`.** The defect this note exists to fix, and worse
  than either a small window or a large budget on its own — the preloader decodes images the cache throws away before
  anyone sees them. The middle rows of the table above are what that costs.
- **A 480 MB floor (`5 × LARGE_DECODE_BYTES`) to preserve `±2` everywhere.** It makes the scaling almost entirely inert:
  even at 1/32 the budget only lands between 480 and 512 MB for RAM between roughly 15 and 16 GB, so every machine
  outside that sliver gets the old flat budget back. The floor's job is to stop the window collapsing, not to pin it.
- **Keeping M0's 1/64 divisor alongside the derived window.** This was the first attempt at the fix, and its own table
  is why it didn't stand: 8 GB → 288 MB/±1, 12 GB → 288 MB/±1, 16 GB → 288 MB/±1, 24 GB → 403 MB/±1, 32 GB → 537 MB/±2.
  It removed the thrashing, but paid for it by narrowing 16 and 24 GB machines that had been fine — charging a fix the
  plan located on an 8 GB laptop to everyone below 32 GB.
- **A divisor of 1/26 or 1/24, so 12 GB gets `±2` too.** Both work arithmetically, but they buy it by spending more on
  the 8 GB machine (330 MB and 358 MB against 288 MB), which is the exact machine the scaling exists to protect.

## Knock-on

`browser::browse_warm_indices` warms the browse selection into this same `ImageCache`, and used to carry its own
`BROWSE_WARM_RADIUS = 2` with a comment saying it matched the preloader. It now takes the radius as a parameter and the
caller passes `preload_count()`, which deletes the duplicated constant and keeps the function pure.

`previews::scheduler::PRELOAD_HALF` deliberately stays at the maximum. It is only a priority boundary for preview
generation, costs no memory, and on the machines where the decode window narrows, having the preview ready matters more.
