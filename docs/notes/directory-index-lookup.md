# Finding the current image in its folder

`DirectoryList::from_file` runs on every launch, before the first pixel. It lists the folder, sorts it, and then has to
say which entry is the image the user opened. That last step used to canonicalize **every** entry and compare, which put
one filesystem call per file on the path between a double-click and an image on screen.

Benchmark: [`benchmarks/directory-index-lookup/`](../../benchmarks/directory-index-lookup). Run it yourself before
trusting these numbers, especially on a share.

## Numbers

2026-08-27, M1 Max, local APFS, release build, synthetic folders, mean of 20 runs. "First / middle / last" is where the
opened image sits in sort order, which matters because `position` short-circuits.

```
                     canonicalize each      compare names      speedup
100 files    first          12.1 µs             10.9 µs            1x
             middle        959.0 µs             18.8 µs           51x
             last         1487.4 µs             16.6 µs           90x
1,000 files  first          11.9 µs             10.6 µs            1x
             middle       5719.5 µs             24.5 µs          234x
             last        11264.6 µs             41.2 µs          273x
5,000 files  first          11.7 µs             10.4 µs            1x
             middle      28668.3 µs             85.1 µs          337x
             last        59154.4 µs            149.3 µs          396x
```

So on the cheapest filesystem this app will ever see, opening a photo from the middle of a 5,000-image folder spent **29
ms** deciding where the cursor goes, and opening the last one spent **59 ms**. The README promises an image on screen in
600 ms.

## Why Windows and SMB are worse, and by how much we don't know

The measurements above are macOS only, and the Windows claim below is a syscall argument rather than a measurement:
nobody has run Prvw on Windows yet.

- **macOS and Linux**: `canonicalize` is `realpath`, one cheap call that mostly hits the kernel's name cache.
- **Windows**: `canonicalize` opens the file (`CreateFileW`), asks for its final name (`GetFinalPathNameByHandleW`), and
  closes it. That's a real file open per entry, and every open passes through whatever filter drivers are installed,
  Defender included. A first read of a folder Defender hasn't seen is the expensive case.
- **Any of them over SMB**: a network round trip per entry. David's photo libraries live on a NAS, so this is the case
  the viewer has to be good at, and it's where a per-file call stops being expensive and starts being unusable.

## What Prvw does now

Every entry in the list came from `read_dir(dir)`, so they all share one parent. That splits the question in two:

1. Is `dir` the folder the canonical target lives in? Free when the caller already passed a canonical path (every launch
   path does), one `canonicalize` when it didn't.
2. Which name in it is the target's? A string comparison per entry, case-folded on Windows.

The per-entry scan stays as the fallback, because names can't answer one case: the opened path is a symlink into a
different folder, so the target's real name isn't in `dir` at all.
`a_symlink_into_another_folder_still_lands_on_the_link` covers it, and reaching the fallback costs exactly what the old
code always cost.

Comparison goes through [`paths`](../../apps/desktop/src/paths.rs) rather than `==`, so `\\?\C:\pics\a.jpg` and
`C:\Pics\A.JPG` are recognized as one file on Windows.
