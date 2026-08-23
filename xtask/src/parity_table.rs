//! Renders the parity registries as the Markdown of `docs/parity.md`.
//!
//! Every fact here comes from `parity::report()`. There is no hand-written entry, no date stamp,
//! and no commit SHA, so the file only changes when the registries do, and the check that
//! compares this output against the committed file has something stable to compare.
//!
//! ## Why lists rather than one big table
//!
//! The doc is for agents (`AGENTS.md`), which read a linear token stream. A 115-row matrix makes
//! every row depend on a header 40 lines up; a line that names its own entry, its own platform,
//! and its own status answers a question on its own, whether it's read in order or grepped out of
//! context. The same data is then rolled up per platform, because "what does Windows still owe?"
//! is the question the file exists to answer.

use crate::parity::{Coverage, Entry, Platform, Registry};
use std::fmt::Write as _;

/// The prose at the top. Generated like everything else, so nobody has to keep it in sync.
const HEADER: &str = "\
# Platform parity

What every platform's UI owes the app, and what it has. Generated from the registries in `apps/desktop/src/parity/`.

Don't edit this file by hand. Edit the registries, then run `./scripts/check.sh --check parity`, which rewrites it. The
check fails on a stale file, so a parity change shows up as a diff here rather than passing unnoticed.

Statuses: `done` is built and reachable on that platform, `not applicable` means the entry is meaningless there (with
the reason, below), and `missing` means it applies but isn't built. Linux is a long column of `missing` on purpose: it
ships without chrome and gets its own spec later (decision 4 in `docs/specs/cross-platform-plan.md`).
";

/// How many entries a platform has in each state.
#[derive(Clone, Copy, Default)]
struct Counts {
    done: usize,
    not_applicable: usize,
    missing: usize,
}

impl Counts {
    fn add(&mut self, coverage: Coverage) {
        match coverage {
            Coverage::Present => self.done += 1,
            Coverage::NotApplicable { .. } => self.not_applicable += 1,
            Coverage::Missing => self.missing += 1,
        }
    }

    fn total(self) -> usize {
        self.done + self.not_applicable + self.missing
    }
}

/// The whole document, ready to write to `docs/parity.md`.
pub fn render(entries: &[Entry]) -> String {
    let mut out = String::with_capacity(32 * 1024);
    out.push_str(HEADER);
    render_summary(&mut out, entries);
    render_gaps(&mut out, entries);
    render_not_applicable(&mut out, entries);
    render_entries(&mut out, entries);
    out
}

fn render_summary(out: &mut String, entries: &[Entry]) {
    out.push_str("\n## Summary\n\n");
    for platform in Platform::ALL {
        let counts = counts_for(entries, *platform, None);
        let _ = writeln!(
            out,
            "- {}: {} of {} done, {} not applicable, {} missing",
            platform.name(),
            counts.done,
            counts.total(),
            counts.not_applicable,
            counts.missing
        );
    }

    out.push_str("\nPer registry, as `done / not applicable / missing`:\n\n");
    for registry in REGISTRIES {
        let total = entries.iter().filter(|e| e.registry == *registry).count();
        let _ = write!(
            out,
            "- {} ({total} {}):",
            plural(*registry),
            entries_word(total)
        );
        for (index, platform) in Platform::ALL.iter().enumerate() {
            let counts = counts_for(entries, *platform, Some(*registry));
            let separator = if index == 0 { " " } else { ", " };
            let _ = write!(
                out,
                "{separator}{} {} / {} / {}",
                platform.name(),
                counts.done,
                counts.not_applicable,
                counts.missing
            );
        }
        out.push('\n');
    }
}

/// What each platform still owes, grouped so a gap reads as a to-do list rather than a wall.
fn render_gaps(out: &mut String, entries: &[Entry]) {
    out.push_str("\n## What each platform still owes\n");
    for platform in Platform::ALL {
        let _ = write!(out, "\n### {}\n\n", platform.name());
        let missing: Vec<&Entry> = entries
            .iter()
            .filter(|entry| coverage_on(entry, *platform) == Coverage::Missing)
            .collect();
        if missing.is_empty() {
            out.push_str("Nothing missing.\n");
            continue;
        }
        let _ = writeln!(out, "{} missing:\n", missing.len());
        for registry in REGISTRIES {
            for group in groups_in(&missing, *registry) {
                let names: Vec<&str> = missing
                    .iter()
                    .filter(|entry| entry.registry == *registry && entry.group == group)
                    .map(|entry| entry.name)
                    .collect();
                let _ = writeln!(
                    out,
                    "- {}, {group}: {}",
                    registry.name(),
                    names
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }
}

/// The declined entries and their reasons. The reasons are the only defence this escape hatch
/// has against becoming a shrug, so they get their own section rather than a truncated cell.
fn render_not_applicable(out: &mut String, entries: &[Entry]) {
    out.push_str("\n## Deliberately not applicable\n");
    let mut any = false;
    for platform in Platform::ALL {
        let declined: Vec<(&Entry, &'static str)> = entries
            .iter()
            .filter_map(|entry| Some((entry, coverage_on(entry, *platform).reason()?)))
            .collect();
        if declined.is_empty() {
            continue;
        }
        any = true;
        let _ = write!(out, "\n### {}\n\n", platform.name());
        for (entry, reason) in declined {
            let _ = writeln!(
                out,
                "- `{}` ({}): {reason}",
                entry.name,
                registry_noun(entry.registry)
            );
        }
    }
    if !any {
        out.push_str("\nNothing is declared not applicable.\n");
    }
}

/// Every entry, one self-describing line each: what it is, where it lives, and every platform's
/// status. Registry order, which is the order the UI presents them in.
fn render_entries(out: &mut String, entries: &[Entry]) {
    out.push_str("\n## Every entry\n");
    for registry in REGISTRIES {
        let _ = write!(out, "\n### {}\n\n", plural(*registry));
        for entry in entries.iter().filter(|entry| entry.registry == *registry) {
            let statuses: Vec<String> = entry
                .coverage
                .iter()
                .map(|(platform, coverage)| format!("{} {}", platform.name(), coverage.status()))
                .collect();
            let _ = writeln!(
                out,
                "- `{}` \"{}\" ({}): {}",
                entry.name,
                entry.label,
                details(entry),
                statuses.join(", ")
            );
        }
    }
}

/// The parenthetical that says what an entry is and where it sits. Exhaustive on purpose: a
/// fourth registry has to decide how it reads here.
fn details(entry: &Entry) -> String {
    match entry.registry {
        Registry::Setting => {
            let field = entry.field.unwrap_or("none");
            format!("setting, {}, {}, field `{field}`", entry.group, entry.kind)
        }
        // `Menu::name()` already carries the word for the context menu, so don't say it twice.
        Registry::MenuItem if entry.group.ends_with("menu") => {
            format!("menu item, {}", entry.group)
        }
        Registry::MenuItem => format!("menu item, {} menu", entry.group),
        Registry::Command => format!("command, {}", entry.group),
    }
}

/// English, for a count that is almost never one but shouldn't read wrong when it is.
fn entries_word(count: usize) -> &'static str {
    if count == 1 { "entry" } else { "entries" }
}

/// The registries, in the order the document lists them.
const REGISTRIES: &[Registry] = &[Registry::Setting, Registry::MenuItem, Registry::Command];

fn plural(registry: Registry) -> &'static str {
    match registry {
        Registry::Setting => "Settings",
        Registry::MenuItem => "Menu items",
        Registry::Command => "Commands",
    }
}

fn registry_noun(registry: Registry) -> &'static str {
    match registry {
        Registry::Setting => "setting",
        Registry::MenuItem => "menu item",
        Registry::Command => "command",
    }
}

fn coverage_on(entry: &Entry, platform: Platform) -> Coverage {
    entry
        .coverage
        .iter()
        .find(|(candidate, _)| *candidate == platform)
        .map(|(_, coverage)| *coverage)
        // `report()` fills in every platform, so this is unreachable. Counting it as missing
        // beats a panic in a documentation generator.
        .unwrap_or(Coverage::Missing)
}

fn counts_for(entries: &[Entry], platform: Platform, registry: Option<Registry>) -> Counts {
    let mut counts = Counts::default();
    for entry in entries {
        if registry.is_some_and(|wanted| entry.registry != wanted) {
            continue;
        }
        counts.add(coverage_on(entry, platform));
    }
    counts
}

/// The groups a registry's entries fall into, in first-seen order, which is registry order.
fn groups_in(entries: &[&Entry], registry: Registry) -> Vec<&'static str> {
    let mut groups: Vec<&'static str> = Vec::new();
    for entry in entries {
        if entry.registry == registry && !groups.contains(&entry.group) {
            groups.push(entry.group);
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One entry with a made-up coverage row per platform, in `Platform::ALL` order.
    fn entry(registry: Registry, name: &'static str, coverage: [Coverage; 3]) -> Entry {
        Entry {
            registry,
            name,
            label: "A label",
            group: "A group",
            kind: match registry {
                Registry::Setting => "toggle",
                Registry::MenuItem => "menu item",
                Registry::Command => "action",
            },
            field: (registry == Registry::Setting).then_some("a_field"),
            coverage: Platform::ALL.iter().copied().zip(coverage).collect(),
        }
    }

    const DECLINED: Coverage = Coverage::NotApplicable {
        reason: "Windows has no such concept, and faking one would be worse than the gap.",
    };

    #[test]
    fn a_not_applicable_entry_renders_with_its_reason() {
        let markdown = render(&[entry(
            Registry::Setting,
            "TitleBar",
            [Coverage::Present, DECLINED, Coverage::Missing],
        )]);
        assert!(
            markdown.contains(
                "- `TitleBar` (setting): Windows has no such concept, and faking one would be \
                 worse than the gap."
            ),
            "the reason should be spelled out under the declining platform:\n{markdown}"
        );
        assert!(
            markdown.contains("macOS done, Windows not applicable, Linux missing"),
            "the entry line should carry every platform's status:\n{markdown}"
        );
        // A declined entry is not a gap: it must not show up in the platform's to-do list.
        assert!(
            !markdown.contains("- Setting, A group: `TitleBar`\n\n### Linux"),
            "a declined entry was listed as owed:\n{markdown}"
        );
    }

    #[test]
    fn a_missing_entry_is_visibly_owed() {
        let markdown = render(&[entry(
            Registry::MenuItem,
            "Print",
            [Coverage::Present, Coverage::Missing, Coverage::Missing],
        )]);
        assert!(
            markdown.contains("### Windows\n\n1 missing:\n\n- Menu item, A group: `Print`\n"),
            "the gap should read as a to-do list:\n{markdown}"
        );
        assert!(
            markdown.contains("### macOS\n\nNothing missing.\n"),
            "a platform with no gaps should say so:\n{markdown}"
        );
        assert!(
            markdown.contains("- `Print` \"A label\" (menu item, A group menu): macOS done"),
            "the entry line should say what the entry is:\n{markdown}"
        );
    }

    #[test]
    fn a_group_that_already_says_menu_isnt_told_twice() {
        let mut context = entry(
            Registry::MenuItem,
            "ContextCopy",
            [Coverage::Present, Coverage::Present, Coverage::Present],
        );
        context.group = "Context menu";
        let markdown = render(&[context]);
        assert!(
            markdown.contains("(menu item, Context menu)"),
            "the group already ends in \"menu\":\n{markdown}"
        );
    }

    #[test]
    fn the_summary_counts_match_the_entries() {
        let markdown = render(&[
            entry(
                Registry::Setting,
                "One",
                [Coverage::Present, Coverage::Missing, Coverage::Missing],
            ),
            entry(
                Registry::Command,
                "Two",
                [Coverage::Present, DECLINED, Coverage::Present],
            ),
        ]);
        assert!(markdown.contains("- macOS: 2 of 2 done, 0 not applicable, 0 missing"));
        assert!(markdown.contains("- Windows: 0 of 2 done, 1 not applicable, 1 missing"));
        assert!(markdown.contains("- Linux: 1 of 2 done, 0 not applicable, 1 missing"));
        assert!(markdown.contains("- Settings (1 entry): macOS 1 / 0 / 0, Windows 0 / 0 / 1"));
        assert!(markdown.contains("- Commands (1 entry): macOS 1 / 0 / 0, Windows 0 / 1 / 0"));
    }

    #[test]
    fn nothing_declined_says_so_rather_than_leaving_an_empty_section() {
        let markdown = render(&[entry(
            Registry::Command,
            "ZoomIn",
            [Coverage::Present, Coverage::Present, Coverage::Present],
        )]);
        assert!(
            markdown.contains(
                "## Deliberately not applicable\n\nNothing is declared not applicable.\n"
            ),
            "an empty section is a question mark; say it's empty:\n{markdown}"
        );
    }

    /// The document is only worth checking in if it accounts for every entry the registries
    /// have, so hold the real report to that.
    #[test]
    fn every_registry_entry_reaches_the_document() {
        let entries = crate::parity::report();
        let markdown = render(&entries);
        for entry in &entries {
            assert!(
                markdown.contains(&format!("- `{}` \"{}\" (", entry.name, entry.label)),
                "{} has no line in the parity table",
                entry.name
            );
        }
        for platform in Platform::ALL {
            let counts = counts_for(&entries, *platform, None);
            assert_eq!(
                counts.total(),
                entries.len(),
                "{} isn't counted for every entry",
                platform.name()
            );
        }
    }

    /// The check compares the file against this output byte for byte, so a second run has to
    /// produce the same bytes.
    #[test]
    fn rendering_is_deterministic() {
        assert_eq!(
            render(&crate::parity::report()),
            render(&crate::parity::report())
        );
    }
}
