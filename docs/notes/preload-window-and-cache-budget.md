# Preload window and image cache budget

Why `navigation::preloader::preload_count()` is derived from the cache budget, why the budget is flat rather than
RAM-proportional, and how the sizing unit was picked. Referenced from `src/navigation/CLAUDE.md`.

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
- RGBA8 rather than RGBA16F even though HDR RAW decodes are 8 bytes per pixel: `HDR_MEMORY_BUDGET` doubles alongside, so
  the window comes out identical in both modes and only one constant is needed.

## The decision: a flat 512 MB budget, and ±2 on every machine

David's call, and the reasoning is his: _"I think the 512 MB floor actually makes sense. We do want to provide
reasonable UX even on low-RAM machines. This affects all machines under 32 GB of RAM and it's fine, let's raise that
floor."_

So `SDR_MEMORY_BUDGET` is a flat 512 MB, `HDR_MEMORY_BUDGET` is twice that, and every machine gets the full ±2 window. A
viewer that navigates instantly is the product; 512 MB is a defensible charge for it even on 8 GB, and an 8 GB machine
needs the latency saved no less than a 32 GB one does.

That made the RAM scaling inert — the ceiling was already 512 MB, so a 512 MB floor left
`clamp(RAM / 64, 512 MB, 512 MB)`, a constant with extra steps. The scaling is gone rather than left in place looking
like it does something.

### Why not keep the scaling and raise the ceiling instead

The idea was that a big machine could retain more previously-viewed images, making backtracking through a folder a cache
hit. **It buys nothing, and the reason is structural rather than a matter of degree.** `App::navigate_by` (`app.rs`)
calls `image_cache.retain_only()` on `current ± preload_count()` after every navigation, so the cache is a sliding
window, not an LRU history: an image two steps behind is dropped one keypress later whatever the budget says. Buying
history depth would mean widening the retention policy, which is a different feature with its own design questions (how
deep? does backtracking actually happen?), not a bigger number.

Meanwhile the window is capped at ±2, so upward the budget has nothing to spend on either way, and downward the only
thing scaling could do was take latency budget from the machines with the least headroom.

RAM-proportional scaling stays right for `previews`, which spends its budget on a ±50 window of ~4 MB thumbnails where
more RAM genuinely buys more of them. `platform::total_physical_ram_bytes()` stays for that reason.

## The window is still derived from the budget

The budget being flat doesn't make `preload_count()` a constant in disguise; it makes the derivation cheap. A window of
`n` holds `2n + 1` images, so the budget has to cover the current image plus `2n` neighbors, and `preload_count()`
computes that rather than restating it. If the budget ever moves, the window follows and can't over-fetch.

That pairing is now checked at **compile time**, in the same spirit as M0.5's parity registries:

```rust
const _: () = assert!(
    (2 * preload_count_for_budget(SDR_MEMORY_BUDGET) + 1) * LARGE_DECODE_BYTES <= SDR_MEMORY_BUDGET
);
```

Verified by breaking it: dropping `SDR_MEMORY_BUDGET` to 200 MB fails the build with
`error[E0080]: evaluation panicked`. Raising `MAX_PRELOAD_AHEAD` can't break it, and that asymmetry is the guarantee —
the derivation simply won't hand out a window the budget doesn't cover, so the cap is only ever the smaller of the two.
The unit test covers the range either side of the shipped budget, so the derivation stays sound if the budget moves.

## What each configuration gets

Images assumed to be 24 MP (the sizing unit). "fetches" is `2n + 1`; "retains" is `budget / 96 MB`.

| RAM          | version      | budget | window | fetches | retains | result        |
| ------------ | ------------ | ------ | ------ | ------- | ------- | ------------- |
| 8 GB         | pre-M0       | 537 MB | ±2     | 5       | 5       | ok            |
| 8 GB         | M0 as merged | 160 MB | ±2     | 5       | 1       | evicts 4 of 5 |
| 8 GB         | now          | 537 MB | ±2     | 5       | 5       | ok            |
| 16 GB        | pre-M0       | 537 MB | ±2     | 5       | 5       | ok            |
| 16 GB        | M0 as merged | 268 MB | ±2     | 5       | 2       | evicts 3 of 5 |
| 16 GB        | now          | 537 MB | ±2     | 5       | 5       | ok            |
| 32 GB and up | any version  | 537 MB | ±2     | 5       | 5       | unchanged     |

So: identical to pre-M0 everywhere, and M0's regression is gone. Folders of phone-sized photos get more than the window
asks for, because the cache charges each entry its real size — a 537 MB budget holds eleven 12 MP decodes.

## Rejected alternatives

Kept because each is a reasonable-looking idea that someone will have again, and the analysis behind them is worth not
repeating.

- **M0 as merged: scale the budget, leave the window at a fixed ±2.** The defect all of this started from, and worse
  than either a small window or a large budget on its own — the preloader decodes images the cache throws away before
  anyone sees them. An 8 GB machine kept one of the five it fetched.
- **A 288 MB floor (`3 × LARGE_DECODE_BYTES`) with a ±1 window on small machines.** Correct as far as it went: it
  removed the thrashing, and 288 MB is genuinely the narrowest budget that keeps navigation warm in both directions.
  Rejected because a narrower window is still a worse viewer, and the RAM it saves isn't worth that on the machines
  where instant navigation matters most.
- **Keeping the scaling but moving the divisor to 1/32**, so 16 GB lands exactly on the 512 MB ceiling and only machines
  below it narrow. Tidy — the divisor stops being arbitrary and says out loud that the flat 512 MB was always tuned for
  a 16 GB machine — and it removed the regression at 16 and 24 GB. Still rejected, because it left 8 and 12 GB machines
  at ±1 for a saving that isn't worth the latency. For the record, its curve was 8 GB → 288 MB/±1, 12 GB → 403 MB/±1, 16
  GB and up → 537 MB/±2.
- **A 1/26 or 1/24 divisor**, to give 12 GB a ±2 window too. Both work arithmetically, but they buy it by spending
  _more_ on the 8 GB machine (330 MB and 358 MB against 288 MB) — the machine the scaling was supposed to protect.
- **Keeping the scaling and raising the ceiling for history depth on big machines.** See above: `retain_only()` makes it
  structurally impossible without a different retention policy.

## Knock-on

`browser::browse_warm_indices` warms the browse selection into this same `ImageCache`, and used to carry its own
`BROWSE_WARM_RADIUS = 2` with a comment saying it matched the preloader. It now takes the radius as a parameter and the
caller passes `preload_count()`, which deletes the duplicated constant and keeps the function pure.

`previews::scheduler::PRELOAD_HALF` is a separate 2 and stays one. It is only a priority boundary for preview generation
and costs no memory, so coupling it to `preload_count()` would buy nothing.
