❌ NEVER tail, head, or truncate the output of `./scripts/check.sh` or any checker script. Run it plain — the output is
already concise and designed to be read in full. Truncating hides the failures you need to see. If a check stalls, fix
the check; don't paper over it with `| tail`.

This duplicates the global rule at `~/.claude/rules/no-tail-checker.md`. Keeping it project-local too because it gets
broken often enough that one source of truth wasn't sticking.
