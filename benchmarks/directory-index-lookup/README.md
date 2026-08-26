# Directory index lookup benchmark

`DirectoryList::from_file` has to answer one question on every launch: which entry of this folder is the image we were
asked to open? This times the two ways of answering it.

- **Canonicalize each** resolves every entry and compares the result to the target. Simple, and the shape Prvw started
  with.
- **Compare names** settles the folder once (at most one `canonicalize`), then matches file names. What Prvw does now,
  with the per-entry scan kept as a fallback for a target that's a symlink into another folder.

## Running

```bash
cargo run --release                     # synthetic folders of 100 / 1,000 / 5,000 files
cargo run --release -- /path/to/folder  # a real folder, on whatever filesystem it lives
```

Point it at a network share to see the case that matters most, and run it on Windows too: the syscall behind
`canonicalize` is `realpath` on macOS and Linux, and `CreateFileW` + `GetFinalPathNameByHandleW` + `CloseHandle` on
Windows, so a file open (and a pass through every installed filter driver) per entry.

Each measurement runs once untimed to warm the OS metadata cache, then 20 more times, and reports the mean. The target's
position in sort order is varied first / middle / last, because `position` short-circuits and that decides the cost.

Results and what they mean: [`docs/notes/directory-index-lookup.md`](../../docs/notes/directory-index-lookup.md).
