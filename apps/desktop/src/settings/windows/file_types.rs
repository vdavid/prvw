//! What "Register Prvw's file types" writes, worked out before anything touches the registry.
//!
//! ## Why this page can't mirror macOS
//!
//! The macOS File associations panel is 16 toggles, one per image type, each calling
//! `LSSetDefaultRoleHandlerForContentType`. **Windows removed programmatic default-handler
//! setting in Windows 10 20H2**: registry edits, `assoc`, and `ftype` are all ignored for the
//! `UserChoice` key, which is hash-protected. No app can make itself the default for `.jpg` any
//! more, and one that claims to is either lying or about to be undone by the OS.
//!
//! So the page does the two things that still work, and says plainly that Windows owns the
//! rest:
//!
//! 1. **Register Prvw's file types**, which is what this module computes: a ProgID, an
//!    `Applications\prvw.exe` entry, an `OpenWithProgids` mention on every extension Prvw
//!    decodes, and a `Capabilities` block listed in `RegisteredApplications`. That's what puts
//!    Prvw in Explorer's "Open with" list and gives it a page of its own in the Windows Settings
//!    picker. The installer writes exactly this list; the button is the repair path for a user
//!    whose registration got clobbered.
//! 2. **Open Windows default apps settings**, a deep link to [`DEFAULT_APPS_URI`], where the
//!    user makes the actual choice.
//!
//! The setting stays `Present` in the parity registry rather than `NotApplicable`, because the
//! capability (choosing what opens in Prvw) is reachable. Only the surface is different.
//!
//! ## Everything here is `HKEY_CURRENT_USER`
//!
//! No elevation, no machine-wide state, and nothing another user can see. [`registration`]
//! returns the writes as data so a Mac can check them, and so the one thing that must never
//! happen (writing `UserChoice`) is checked by a test rather than by a reviewer's memory.
//!
//! ## One list, two writers
//!
//! [`registration`] and [`removal`] are the whole story of what Prvw owns in the registry, and
//! both the settings button and the Windows installer read them rather than keeping a list each.
//! The installer can't link this crate, so `cargo xtask installer-registry` renders the same two
//! functions into the NSIS include the installer compiles
//! (`apps/desktop/installer/windows/file-associations.nsh`), and the `installer` check fails when
//! the committed include and this module disagree.

use std::path::Path;

/// The ProgID Prvw registers its file types under. Explorer shows the app, not this, but it's
/// the key everything else hangs off.
pub const PROG_ID: &str = "Prvw.Image";

/// The application key Explorer's "Open with" list reads. It has to be the executable's file
/// name, which is what makes this a constant rather than something derived at runtime.
pub const APPLICATION_KEY: &str = "prvw.exe";

/// The friendly name Explorer shows beside the icon.
pub const FRIENDLY_NAME: &str = "Prvw";

/// What a registered file reads as in Explorer's Type column.
pub const PROG_ID_DESCRIPTION: &str = "Image";

/// The one line Windows Settings shows under Prvw's name on its default-apps page.
pub const APPLICATION_DESCRIPTION: &str = "A fast, minimal image viewer.";

/// Prvw's `Capabilities` key, and the value `RegisteredApplications` points at. Listing it is
/// what gives Prvw a page of its own in Settings instead of only an "Open with" entry.
const CAPABILITIES_KEY: &str = r"Software\Prvw\Capabilities";

/// The key Prvw's whole `Capabilities` subtree hangs off, and what the uninstaller removes.
///
/// Dead code to the app, and that's the point: [`removal`] is its only reader, and [`removal`]'s
/// only consumer is an NSIS script. Nothing in Prvw ever unregisters Prvw.
#[allow(dead_code)]
const VENDOR_KEY: &str = r"Software\Prvw";

/// Where an app announces that it has a `Capabilities` key worth reading.
const REGISTERED_APPLICATIONS_KEY: &str = r"Software\RegisteredApplications";

/// Where the user actually picks their default image viewer. The whole reason the second
/// button exists.
pub const DEFAULT_APPS_URI: &str = "ms-settings:defaultapps";

/// One string value to write under `HKEY_CURRENT_USER`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryValue {
    /// The key path, relative to `HKEY_CURRENT_USER`.
    pub key: String,
    /// The value's name, or `None` for the key's unnamed default value.
    pub name: Option<String>,
    /// The string to store. Empty is meaningful: that's how `OpenWithProgids` and
    /// `SupportedTypes` mark membership.
    pub value: String,
}

impl RegistryValue {
    fn default_value(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            name: None,
            value: value.into(),
        }
    }

    fn named(key: impl Into<String>, name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            name: Some(name.into()),
            value: value.into(),
        }
    }
}

/// Every extension Prvw opens, dotted and lower case, in the order the decoder lists them.
///
/// Read from `decoding::supported_extensions` rather than written down again, so a format can't
/// be openable and yet unregistered.
pub fn extensions() -> Vec<String> {
    let mut extensions: Vec<String> = crate::decoding::supported_extensions()
        .into_iter()
        .map(|extension| format!(".{extension}"))
        .collect();
    extensions.sort();
    extensions
}

/// The extension list as the page shows it: upper case without the dot, comma separated, the
/// way Windows itself lists file types.
pub fn extension_list_text() -> String {
    extensions()
        .iter()
        .map(|extension| extension.trim_start_matches('.').to_uppercase())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The command line Explorer runs to open a file with Prvw.
///
/// Quoted because a path with a space in it (`C:\Program Files\…`) would otherwise arrive as
/// two arguments, and `%1` is quoted for the same reason on the file's side.
fn open_command(executable: &str) -> String {
    format!("\"{executable}\" \"%1\"")
}

/// Everything "Register Prvw's file types" writes, in the order it writes it.
///
/// `executable` is the full path to `prvw.exe` in its plain Win32 spelling: `paths::shell_path`
/// is what produces one, because a verbatim `\\?\` path in a registry command line is not
/// something the shell will run.
pub fn registration(executable: &Path) -> Vec<RegistryValue> {
    let executable = executable.to_string_lossy().to_string();
    let command = open_command(&executable);
    let classes = r"Software\Classes";
    let application = format!(r"{classes}\Applications\{APPLICATION_KEY}");

    let mut writes = vec![
        // The ProgID: what a file of ours is called, what it looks like, and how to open it.
        RegistryValue::default_value(format!(r"{classes}\{PROG_ID}"), PROG_ID_DESCRIPTION),
        RegistryValue::default_value(
            format!(r"{classes}\{PROG_ID}\DefaultIcon"),
            format!("{executable},0"),
        ),
        RegistryValue::default_value(format!(r"{classes}\{PROG_ID}\shell\open\command"), &command),
        // The application entry, which is the half Explorer's "Open with" list reads.
        RegistryValue::named(&application, "FriendlyAppName", FRIENDLY_NAME),
        RegistryValue::default_value(format!(r"{application}\shell\open\command"), &command),
        // The `Capabilities` block Settings reads to give Prvw a page of its own.
        RegistryValue::named(CAPABILITIES_KEY, "ApplicationName", FRIENDLY_NAME),
        RegistryValue::named(
            CAPABILITIES_KEY,
            "ApplicationDescription",
            APPLICATION_DESCRIPTION,
        ),
        RegistryValue::named(
            CAPABILITIES_KEY,
            "ApplicationIcon",
            format!("{executable},0"),
        ),
    ];

    for extension in extensions() {
        // Membership, not ownership: `OpenWithProgids` offers Prvw in the "Open with" list and
        // in the Windows Settings picker. What actually opens the file is `UserChoice`, which
        // only the user can set (see the module docs).
        writes.push(RegistryValue::named(
            format!(r"{classes}\{extension}\OpenWithProgids"),
            PROG_ID,
            "",
        ));
        writes.push(RegistryValue::named(
            format!(r"{application}\SupportedTypes"),
            &extension,
            "",
        ));
        writes.push(RegistryValue::named(
            format!(r"{CAPABILITIES_KEY}\FileAssociations"),
            &extension,
            PROG_ID,
        ));
    }

    // Last, because it's the announcement: until this value exists, Settings doesn't look for
    // the `Capabilities` key the lines above just finished writing.
    writes.push(RegistryValue::named(
        REGISTERED_APPLICATIONS_KEY,
        FRIENDLY_NAME,
        CAPABILITIES_KEY,
    ));
    writes
}

/// One thing to take back out of `HKEY_CURRENT_USER` when Prvw is uninstalled.
///
/// The three shapes exist because Prvw owns some keys outright and is only a guest in others:
/// `Software\Classes\Prvw.Image` is ours to delete, while `Software\Classes\.jpg` belongs to
/// whoever else registered the extension and we take our one value out of it.
///
/// Unused by the app, like [`removal`] itself. See that function.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryRemoval {
    /// A key Prvw created, with everything under it.
    KeyTree(String),
    /// One value out of a key that isn't ours.
    Value {
        /// The key path, relative to `HKEY_CURRENT_USER`.
        key: String,
        /// The value's name.
        name: String,
    },
    /// A key to delete only if nothing is left in it, which is how the shared extension keys get
    /// tidied up without taking another app's registration with them.
    KeyIfEmpty(String),
}

/// Everything an uninstall takes back out, in the order it goes.
///
/// The exact inverse of [`registration`], and `an_uninstall_takes_back_every_key_it_wrote` is the
/// test that keeps it that way.
///
/// **Nothing in the app calls this, on purpose.** Prvw never unregisters itself; the uninstaller
/// does, and the uninstaller is an NSIS script that reads this through
/// `cargo xtask installer-registry`. It lives here rather than in the installer so the writes and
/// their inverse stay one file apart from each other, and so a Mac can test both.
#[allow(dead_code)]
pub fn removal() -> Vec<RegistryRemoval> {
    let classes = r"Software\Classes";

    // The announcement goes first: a `RegisteredApplications` entry pointing at a `Capabilities`
    // key that no longer exists is what makes Settings show an empty app page.
    let mut removals = vec![
        RegistryRemoval::Value {
            key: REGISTERED_APPLICATIONS_KEY.to_string(),
            name: FRIENDLY_NAME.to_string(),
        },
        RegistryRemoval::KeyTree(VENDOR_KEY.to_string()),
        RegistryRemoval::KeyTree(format!(r"{classes}\{PROG_ID}")),
        RegistryRemoval::KeyTree(format!(r"{classes}\Applications\{APPLICATION_KEY}")),
    ];

    for extension in extensions() {
        let open_with = format!(r"{classes}\{extension}\OpenWithProgids");
        removals.push(RegistryRemoval::Value {
            key: open_with.clone(),
            name: PROG_ID.to_string(),
        });
        removals.push(RegistryRemoval::KeyIfEmpty(open_with));
        removals.push(RegistryRemoval::KeyIfEmpty(format!(
            r"{classes}\{extension}"
        )));
    }
    removals
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writes() -> Vec<RegistryValue> {
        registration(Path::new(r"C:\Program Files\Prvw\prvw.exe"))
    }

    /// The one thing this must never do. `UserChoice` is the key that decides what actually
    /// opens a file, Windows hash-protects it, and an app writing there either fails or gets
    /// its association reset by the OS with a "an app default was reset" notification. If this
    /// test ever fails, someone has tried to fake the thing the page's copy says we can't do.
    #[test]
    fn nothing_touches_the_user_choice() {
        for write in writes() {
            assert!(
                !write.key.to_lowercase().contains("userchoice"),
                "{} writes UserChoice, which Windows owns",
                write.key
            );
        }
        for removal in removal() {
            let key = match &removal {
                RegistryRemoval::KeyTree(key) | RegistryRemoval::KeyIfEmpty(key) => key,
                RegistryRemoval::Value { key, .. } => key,
            };
            assert!(
                !key.to_lowercase().contains("userchoice"),
                "{key} deletes UserChoice, which Windows owns"
            );
        }
    }

    /// Everything lands under the current user's own `Software` hive, which is what makes the
    /// whole registration elevation-free. A viewer that asked for admin rights to claim `.jpg`
    /// would be a viewer people stop installing, and the installer relies on this: it runs
    /// `asInvoker` and would fail outright on a machine-wide key.
    #[test]
    fn everything_is_per_user() {
        for write in writes() {
            assert!(
                write.key.starts_with(r"Software\"),
                "{} is outside the user's own software hive",
                write.key
            );
        }
        for removal in removal() {
            let key = match &removal {
                RegistryRemoval::KeyTree(key) | RegistryRemoval::KeyIfEmpty(key) => key,
                RegistryRemoval::Value { key, .. } => key,
            };
            assert!(
                key.starts_with(r"Software\"),
                "{key} is outside the user's own software hive"
            );
        }
    }

    /// Windows Settings only looks for a `Capabilities` key once an app has announced one, so
    /// the announcement and the key it points at have to agree exactly.
    #[test]
    fn the_capabilities_key_is_announced_where_settings_looks() {
        let writes = writes();
        let announcement = writes
            .iter()
            .find(|write| write.key == r"Software\RegisteredApplications")
            .expect("Prvw announces itself in RegisteredApplications");
        assert_eq!(announcement.name.as_deref(), Some("Prvw"));

        let capabilities = &announcement.value;
        assert!(
            writes.iter().any(|write| &write.key == capabilities
                && write.name.as_deref() == Some("ApplicationName")),
            "{capabilities} is announced but never written"
        );
        for extension in extensions() {
            assert!(
                writes.iter().any(
                    |write| write.key == format!(r"{capabilities}\FileAssociations")
                        && write.name.as_deref() == Some(&extension)
                        && write.value == PROG_ID
                ),
                "{extension} is missing from the Capabilities file associations"
            );
        }
    }

    /// An uninstall leaves nothing of Prvw's behind. Every key the registration creates is
    /// either deleted outright or, for the extension keys we share with other apps, emptied of
    /// our value and then dropped only if it's empty.
    #[test]
    fn an_uninstall_takes_back_every_key_it_wrote() {
        let removals = removal();
        let covered = |key: &str, name: Option<&str>| {
            removals.iter().any(|removal| match removal {
                RegistryRemoval::KeyTree(tree) => {
                    key == tree || key.starts_with(&format!(r"{tree}\"))
                }
                RegistryRemoval::Value {
                    key: value_key,
                    name: value_name,
                } => key == value_key && name == Some(value_name.as_str()),
                RegistryRemoval::KeyIfEmpty(_) => false,
            })
        };
        for write in writes() {
            assert!(
                covered(&write.key, write.name.as_deref()),
                "{} / {:?} survives an uninstall",
                write.key,
                write.name
            );
        }
    }

    /// A key another app also registered in must never be deleted outright: `.jpg` is shared,
    /// and taking the whole key would unregister every other viewer on the machine.
    #[test]
    fn a_shared_extension_key_is_only_ever_deleted_when_empty() {
        for removal in removal() {
            if let RegistryRemoval::KeyTree(key) = removal {
                assert!(
                    !key.contains(r"Classes\."),
                    "{key} is a shared extension key, so it can't be deleted outright"
                );
            }
        }
    }

    /// Every format the decoder opens is offered in Explorer's "Open with", both ways round:
    /// the extension mentions the ProgID, and the application says it supports the extension.
    #[test]
    fn every_decodable_format_is_registered() {
        let writes = writes();
        for extension in extensions() {
            let offered = writes.iter().any(|write| {
                write.key == format!(r"Software\Classes\{extension}\OpenWithProgids")
                    && write.name.as_deref() == Some(PROG_ID)
            });
            assert!(offered, "{extension} isn't offered in Open with");

            let supported = writes.iter().any(|write| {
                write.key.ends_with(r"\SupportedTypes") && write.name.as_deref() == Some(&extension)
            });
            assert!(supported, "{extension} isn't in SupportedTypes");
        }
        assert!(extensions().contains(&".cr3".to_string()));
        assert!(extensions().contains(&".jpg".to_string()));
        assert!(!extensions().contains(&".txt".to_string()));
    }

    /// A path with a space in it is the normal case (`C:\Program Files\…`), so both halves of
    /// the command line are quoted. Getting this wrong opens Prvw with the file name split in
    /// two, and it only shows up on a machine whose install path has a space.
    #[test]
    fn the_open_command_survives_a_space_in_the_path() {
        let command = writes()
            .into_iter()
            .find(|write| {
                write
                    .key
                    .ends_with(r"Applications\prvw.exe\shell\open\command")
            })
            .expect("the application's open command");
        assert_eq!(
            command.value,
            r#""C:\Program Files\Prvw\prvw.exe" "%1""#.to_string()
        );
        assert_eq!(command.name, None, "it's the key's default value");
    }

    /// The icon comes out of the executable's own resources, which `build.rs` already embeds.
    #[test]
    fn the_icon_comes_from_the_executable() {
        let icon = writes()
            .into_iter()
            .find(|write| write.key.ends_with(r"\DefaultIcon"))
            .expect("a default icon");
        assert_eq!(icon.value, r"C:\Program Files\Prvw\prvw.exe,0");
    }

    /// No key and name is written twice, which would mean two answers for one question and a
    /// coin toss over which lands last.
    #[test]
    fn nothing_is_written_twice() {
        let writes = writes();
        for (index, write) in writes.iter().enumerate() {
            let duplicate = writes[index + 1..]
                .iter()
                .find(|other| other.key == write.key && other.name == write.name);
            assert!(
                duplicate.is_none(),
                "{} / {:?} is written twice",
                write.key,
                write.name
            );
        }
    }

    /// The list the page shows names every format, in the shape Windows lists file types.
    #[test]
    fn the_list_reads_like_windows_lists_file_types() {
        let text = extension_list_text();
        assert!(text.starts_with("ARW, BMP, "), "got {text}");
        assert!(text.contains("JPG"));
        assert!(!text.contains('.'));
    }
}
