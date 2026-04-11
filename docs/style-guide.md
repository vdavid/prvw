# Style guide

Writing and code styles.

## Writing

- Wording
  - **Use a friendly style**: Make all texts informal, friendly, encouraging, and concise.
  - **Use active voice**: Always prefer active voice. "We moved your files" not "Your files were moved." This is
    especially important for success messages, error messages, and UI copy. Passive voice creeps in. Watch for it.
  - **Abbreviate English**: Use "I'm", "don't", and such.
  - **Don't trivialize**: Avoid terminology of "just", "simple", "easy", and "all you have to do".
  - **Use gender-neutral language**: Use they/them rather than he/him/she/her. Use "folks" or "everyone" rather than
    "guys".
  - **Use universally understood terms**: Use "start" instead of "kickoff", and "end" instead of "wrap up".
  - **Avoid ableist language**: "placeholder value" rather than "dummy value". No "lame", "sanity check" which derive
    from disabilities.
  - **Avoid violent terms**: "stop a process" rather than "kill" or "nuke" it.
  - **Avoid exclusionary terminology**: Prefer "primary/secondary" or "main/replica" over "master/slave". Use
    "allowlist/denylist" over "whitelist/blacklist".
  - **Use verbs, not verb-noun phrases**: "Search" not "Make a search". "Save" not "Perform a save".
  - **Don't use permissive language**: Give users confidence. "Open the image and start browsing" not "Open the image
    and you can start browsing."
  - **Be mindful of user expertise**: Avoid jargon. Link to definitions and explain concepts when necessary.
  - **Avoid latinisms**: For example, use "for example" instead of "e.g.".
  - **Avoid abbreviations**: Very common acronyms like "URL" are okay.
  - **Some casual terms are okay**: Use "docs", not "documentation". Use "dev" for developer and "gen" for generation
    where appropriate and understandable.
- Punctuation, capitalization, numbers
  - **Use sentence case in titles**: Regardless whether visible on the UI or dev only.
  - **Use sentence case in labels**: Applies to buttons, labels, and similar. But omit periods on short microcopy.
  - **Capitalize names correctly**: For example, there is GitHub but mailcow.
  - **Use the Oxford comma**: Use "1, 2, and 3" rather than "1, 2 and 3".
  - **Use en dashes but no em dashes**: en dash for ranges, but avoid structures that'd need an em dash.
  - **Use colon for lists**: Use the format I used in this list you're reading right now.
  - **Spell out numbers one through nine.** Use numerals for 10+.
  - **Use ISO dates**: Use YYYY-MM-DD wherever it makes sense.
- UI
  - **Error messages**: Keep conversational, positive, actionable, and specific. Never use the words "error" or "failed".
    Suggest a next step.
    - "Couldn't open the image. The file might be corrupted." not "Error: Failed to decode image."
  - **Window title**: Show the filename (not the full path) while viewing. Show the app name when no image is open.

## Code

### Comments

Only add doc comments that actually add info. No tautologies.

- ✅ Add meaningful comments for public functions, methods, and types to help the next dev.
- ❌ DO NOT write `Gets the zoom level` for a function called `get_zoom_level`.
- ⚠️ Before adding a doc comment, try using a more descriptive name for the function/param/variable.
- ✅ USE doc comments to mark caveats, tricky/unusual solutions, formats, and constraints.

### Rust

- Max 120 char lines, 4-space indent, cognitive complexity threshold: 15, enforced by clippy.
- Use `snake_case` for variables, functions, and modules. `PascalCase` for types and traits. `SCREAMING_SNAKE_CASE` for
  constants.
- Prefer `&str` over `String` in function parameters when ownership isn't needed.
- Use `thiserror` for library-style error types. Use `anyhow` for application-level error handling where you don't need
  to match on specific variants.
- Keep `unsafe` to an absolute minimum. Document every `unsafe` block with a `// SAFETY:` comment explaining why it's
  sound.
- Put constants closest to where they're used. If only used in one function, put it in that function.

## Design

See [design-principles.md](design-principles.md) for product design values (speed, simplicity, native feel,
accessibility). Read it when designing features or making UX decisions.
