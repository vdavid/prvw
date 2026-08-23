//! The parity registries: what every platform's UI owes the app, checked by the compiler.
//!
//! Prvw forks its chrome per operating system (`docs/design-principles.md`, and the decision
//! record in `docs/specs/cross-platform-plan.md`), so the same feature gets built once per
//! platform. This module is how "are they the same?" becomes a question the build answers.
//!
//! ## The shape
//!
//! Three registries name what exists, one entry per user-facing thing:
//!
//! - [`setting_keys::SettingKey`] — every persisted setting a UI has to expose.
//! - [`menu_items::MenuItemKey`] — every menu item.
//! - [`command_keys::CommandKey`] — every action `AppCommand` can carry out.
//!
//! Each registry answers [`Coverage`] per [`Platform`] through an exhaustive `match` with no
//! `_` arm. Add an entry and every platform's match stops compiling until it says what that
//! platform does with it: [`Coverage::Present`], [`Coverage::NotApplicable`] with a reason, or
//! [`Coverage::Missing`]. That is the guarantee this layer exists for.
//!
//! ## Why it isn't bookkeeping
//!
//! A registry nobody consults rots into fiction within a month, so the entries are load-bearing
//! in the UI code itself:
//!
//! - Settings rows are built from a `SettingKey` (`settings::widgets::make_setting_row` and the
//!   RAW panel's row factories take one). The row's title is [`setting_keys::SettingKey::label`],
//!   so there is no way to put a row on screen without naming its key.
//! - Menu items are built from a `MenuItemKey`, whose label the item wears and whose
//!   [`menu_items::MenuItemKey::command`] is what a click dispatches.
//! - [`Audit`] then checks the built set against the declared one while the UI is being built,
//!   so a `Present` a platform doesn't honour shows up the first time that UI opens.
//!
//! ## Everything here compiles everywhere
//!
//! Nothing here is `#[cfg]`ed out, deliberately. A macOS build knows what Windows owes, which
//! is what makes `cargo check` on one machine catch a missing arm for another, and what lets
//! one host generate the whole parity table (layer 2) rather than a slice of it. The `#[cfg]`s
//! stay in the UI modules that build widgets. What a given build's own UI doesn't consume is
//! unused there by construction (Linux has no menu bar and no settings window at all), so the
//! `mod parity;` declaration in `main.rs` carries one dead-code allow for the platforms
//! without chrome. macOS, which has all of it, still gets the warning.
//!
//! The tree is also dependency-free (`core` and `std` only), so
//! `tests/parity_registries.rs` can load it with `#[path]` and prove that a match missing an
//! arm fails to compile.
//!
//! ## Layers 2 and 3
//!
//! [`report`] hands layer 2 (the generated `docs/parity.md`) the whole table as owned data:
//! entry, label, group, and each platform's status, with `NotApplicable` reasons carried
//! through. Layer 3 (the shared behavioural E2E suite) is separate and lives in `tests/`.

pub mod command_keys;
pub mod menu_items;
pub mod setting_keys;

use command_keys::CommandKey;
use menu_items::MenuItemKey;
use setting_keys::SettingKey;

/// A platform Prvw builds chrome for.
///
/// Linux is here because it ships and has to stay honest about what it lacks, not because it
/// gets parity work. See decision 4 in `docs/specs/cross-platform-plan.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
}

impl Platform {
    /// Every platform, in the order the parity table lists them.
    pub const ALL: &'static [Platform] = &[Platform::MacOs, Platform::Windows, Platform::Linux];

    /// The platform this build targets. Used by [`Audit`] to check the running UI against what
    /// the registry says it should have built.
    pub const HOST: Platform = if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Linux
    };

    /// Display name, spelled the way the platform spells itself.
    pub const fn name(self) -> &'static str {
        match self {
            Platform::MacOs => "macOS",
            Platform::Windows => "Windows",
            Platform::Linux => "Linux",
        }
    }
}

/// What one platform's UI does with one registry entry.
///
/// [`Coverage::NotApplicable`] is the escape hatch, and it is the one thing that can rot this
/// whole layer: a reason like "n/a" or "doesn't fit" turns a compile error into a shrug, and
/// the next person copies it. A reason has to name the platform fact that makes the entry
/// meaningless there (a window model, an OS convention, an API that doesn't exist). "We haven't
/// built it yet" is [`Coverage::Missing`], which the parity table reports as a gap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    /// Built and reachable on this platform.
    Present,
    /// Genuinely meaningless here. The reason is data: layer 2 renders it in the table.
    NotApplicable { reason: &'static str },
    /// Applies here, but isn't built yet.
    Missing,
}

impl Coverage {
    /// The status word layer 2 puts in the table.
    pub const fn status(self) -> &'static str {
        match self {
            Coverage::Present => "done",
            Coverage::NotApplicable { .. } => "not applicable",
            Coverage::Missing => "missing",
        }
    }

    /// The reason a `NotApplicable` carries, for the table's second column.
    pub const fn reason(self) -> Option<&'static str> {
        match self {
            Coverage::NotApplicable { reason } => Some(reason),
            Coverage::Present | Coverage::Missing => None,
        }
    }
}

/// Which registry an [`Entry`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Registry {
    Setting,
    MenuItem,
    Command,
}

impl Registry {
    pub const fn name(self) -> &'static str {
        match self {
            Registry::Setting => "Setting",
            Registry::MenuItem => "Menu item",
            Registry::Command => "Command",
        }
    }
}

/// One row of the parity table, flattened for layer 2.
#[derive(Clone, Debug)]
pub struct Entry {
    pub registry: Registry,
    /// Stable identifier: the registry variant's name. Safe to sort and diff on.
    pub name: &'static str,
    /// What the user sees, or what the action is called.
    pub label: &'static str,
    /// Where it sits: the settings panel, the menu, or the command's area.
    pub group: &'static str,
    /// What sort of thing it is: the control a setting needs, or the registry's own noun.
    pub kind: &'static str,
    /// For a setting, the `Settings` field it drives, as a dotted path into the settings JSON.
    /// The other registries have nothing persisted behind them.
    pub field: Option<&'static str>,
    /// Every platform's status, in [`Platform::ALL`] order.
    pub coverage: Vec<(Platform, Coverage)>,
}

/// The whole parity table, as owned data.
///
/// Layer 2 (`docs/parity.md` and the check that regenerates it) reads this. It's a plain
/// function of the registries, so it gives the same answer on every host.
pub fn report() -> Vec<Entry> {
    let mut entries = Vec::new();
    for key in SettingKey::ALL {
        entries.push(Entry {
            registry: Registry::Setting,
            name: key.name(),
            label: key.label(),
            group: key.panel().name(),
            kind: key.control().name(),
            field: Some(key.field()),
            coverage: coverage_by_platform(|platform| key.coverage(platform)),
        });
    }
    for key in MenuItemKey::ALL {
        entries.push(Entry {
            registry: Registry::MenuItem,
            name: key.name(),
            label: key.label(),
            group: key.menu().name(),
            kind: "menu item",
            field: None,
            coverage: coverage_by_platform(|platform| key.coverage(platform)),
        });
    }
    for key in CommandKey::ALL {
        entries.push(Entry {
            registry: Registry::Command,
            name: key.name(),
            label: key.label(),
            group: key.area().name(),
            kind: "action",
            field: None,
            coverage: coverage_by_platform(|platform| key.coverage(platform)),
        });
    }
    entries
}

fn coverage_by_platform(of: impl Fn(Platform) -> Coverage) -> Vec<(Platform, Coverage)> {
    Platform::ALL
        .iter()
        .map(|platform| (*platform, of(*platform)))
        .collect()
}

/// A mismatch between what a platform declared and what its UI built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mismatch<K> {
    /// The registry says this platform has it; the UI never built it.
    Declared(K),
    /// The UI built it; the registry doesn't say this platform has it.
    Undeclared(K, Coverage),
}

/// Collects the registry entries a platform's UI actually built, so a [`Coverage::Present`]
/// can be checked against reality rather than taken on faith.
///
/// The UI records each entry as it builds it and calls [`Audit::mismatches`] when it's done.
/// The compiler can't see inside a widget factory, so this is the runtime half of the
/// guarantee: it catches a `Present` nobody honoured and a widget nobody declared.
pub struct Audit<K> {
    built: Vec<K>,
}

impl<K: Copy + PartialEq> Default for Audit<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Copy + PartialEq> Audit<K> {
    pub fn new() -> Self {
        Self { built: Vec::new() }
    }

    /// Note that the UI built this entry. Called from the widget factories themselves, so a
    /// row can't reach the screen without being recorded.
    pub fn record(&mut self, key: K) {
        self.built.push(key);
    }

    /// Compare what was built against what `declared` says this platform owes.
    ///
    /// `declared` is the registry's own answer for one platform: every entry with its
    /// coverage. An empty result means the declaration is honest.
    pub fn mismatches(
        &self,
        declared: impl IntoIterator<Item = (K, Coverage)>,
    ) -> Vec<Mismatch<K>> {
        let mut out = Vec::new();
        let mut known = Vec::new();
        for (key, coverage) in declared {
            known.push((key, coverage));
            if coverage == Coverage::Present && !self.built.contains(&key) {
                out.push(Mismatch::Declared(key));
            }
        }
        for key in &self.built {
            match known.iter().find(|(candidate, _)| candidate == key) {
                Some((_, Coverage::Present)) => {}
                Some((_, coverage)) => out.push(Mismatch::Undeclared(*key, *coverage)),
                // Not in the registry at all. Can't happen while `declared` enumerates
                // `ALL`, but reporting it beats silence if a caller passes a subset.
                None => out.push(Mismatch::Undeclared(*key, Coverage::Missing)),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_covers_every_registry_on_every_platform() {
        let entries = report();
        let expected = SettingKey::ALL.len() + MenuItemKey::ALL.len() + CommandKey::ALL.len();
        assert_eq!(entries.len(), expected);
        for entry in &entries {
            assert_eq!(entry.coverage.len(), Platform::ALL.len(), "{}", entry.name);
            assert!(!entry.label.is_empty(), "{}", entry.name);
        }
    }

    /// The escape hatch is only worth having while the reasons are real, so hold them to a
    /// shape a human wrote: a sentence, not a shrug.
    #[test]
    fn not_applicable_reasons_say_something() {
        for entry in report() {
            for (platform, coverage) in entry.coverage {
                let Some(reason) = coverage.reason() else {
                    continue;
                };
                assert!(
                    reason.len() >= 25,
                    "{} on {}: reason is too thin to be a reason: {reason:?}",
                    entry.name,
                    platform.name()
                );
            }
        }
    }

    #[test]
    fn audit_reports_both_directions() {
        let audit = {
            let mut audit = Audit::new();
            audit.record("built");
            audit.record("undeclared");
            audit
        };
        let mismatches = audit.mismatches([
            ("built", Coverage::Present),
            ("never-built", Coverage::Present),
            ("undeclared", Coverage::Missing),
            ("elsewhere", Coverage::NotApplicable { reason: "n/a" }),
        ]);
        assert_eq!(
            mismatches,
            vec![
                Mismatch::Declared("never-built"),
                Mismatch::Undeclared("undeclared", Coverage::Missing),
            ]
        );
    }

    #[test]
    fn honest_audit_has_no_mismatches() {
        let mut audit = Audit::new();
        audit.record(1);
        assert!(
            audit
                .mismatches([
                    (1, Coverage::Present),
                    (2, Coverage::NotApplicable { reason: "why not" }),
                    (3, Coverage::Missing),
                ])
                .is_empty()
        );
    }
}
