//! What the About box says, as data.
//!
//! Every platform builds its own About window with its own toolkit (`docs/design-principles.md`
//! forks the chrome by OS), and the one thing they must not fork is what the box claims about
//! the product. So the strings live here, once, and each presentation reads them: `macos.rs`
//! lays them out in an `NSStackView`, `windows.rs` in a Win32 popup.
//!
//! [`AboutContent::for_platform`] takes the platform rather than reading `cfg!`, which is what
//! lets a Mac assert Windows' copy (`src/scroll.rs` and `src/paths.rs` are the precedent).

use crate::parity::Platform;

/// Where the licence lives. The repository is public, so this is the licence text itself rather
/// than a page about it.
const LICENSE_URL: &str = "https://github.com/vdavid/prvw/blob/main/LICENSE";

/// One piece of clickable text.
pub struct Link {
    pub label: &'static str,
    pub url: &'static str,
}

/// A sentence with exactly one link inside it.
///
/// Kept split rather than pre-rendered because the two presentations need it differently: a
/// `SysLink` wants markup, and AppKit wants the three pieces as separate views.
pub struct LicenseLine {
    pub prefix: &'static str,
    pub link: Link,
    pub suffix: &'static str,
}

impl LicenseLine {
    /// The sentence with no markup. What a person reads, whatever renders it.
    #[cfg(test)]
    fn plain(&self) -> String {
        format!("{}{}{}", self.prefix, self.link.label, self.suffix)
    }

    /// The same sentence as `SysLink` markup, which is a tiny HTML subset: one `<a href="…">`
    /// per link and no entities. Escaping `&` and `<` keeps a label with either in it from
    /// being read as a mnemonic or as the start of a tag.
    pub fn markup(&self) -> String {
        format!(
            "{}<a href=\"{}\">{}</a>{}",
            escape(self.prefix),
            escape(self.link.url),
            escape(self.link.label),
            escape(self.suffix)
        )
    }
}

/// `SysLink` parses `&` and `<` itself, so anything that isn't ours has to be spelled out.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;")
}

/// Everything the About box shows, for one platform.
pub struct AboutContent {
    pub window_title: &'static str,
    pub name: &'static str,
    /// Already prefixed, because that's how both presentations show it.
    pub version: &'static str,
    pub tagline: &'static str,
    pub author: &'static str,
    pub author_site: Link,
    pub license: LicenseLine,
    pub website: Link,
}

impl AboutContent {
    /// What the About box says on `platform`.
    ///
    /// Only the tagline forks, and it forks because naming the wrong operating system in the
    /// product's own About box is the kind of detail a ported app gets wrong.
    pub fn for_platform(platform: Platform) -> Self {
        Self {
            window_title: "About Prvw",
            name: "Prvw",
            version: concat!("Version ", env!("CARGO_PKG_VERSION")),
            tagline: match platform {
                Platform::MacOs => "A fast image viewer for macOS.",
                Platform::Windows => "A fast image viewer for Windows.",
                Platform::Linux => "A fast image viewer for Linux.",
            },
            author: "By David Veszelovszki",
            author_site: Link {
                label: "veszelovszki.com",
                url: "https://veszelovszki.com",
            },
            license: LicenseLine {
                prefix: "Free forever for personal use, under the ",
                link: Link {
                    label: "Business Source License 1.1",
                    url: LICENSE_URL,
                },
                suffix: ".",
            },
            website: Link {
                label: "getprvw.com",
                url: "https://getprvw.com",
            },
        }
    }

    /// What this build's own About box says.
    pub fn host() -> Self {
        Self::for_platform(Platform::HOST)
    }

    /// Every string a person reads in the box, for the copy checks below.
    #[cfg(test)]
    fn user_visible_strings(&self) -> Vec<String> {
        vec![
            self.window_title.to_string(),
            self.name.to_string(),
            self.version.to_string(),
            self.tagline.to_string(),
            self.author.to_string(),
            self.author_site.label.to_string(),
            self.license.plain(),
            self.website.label.to_string(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The box names the operating system it's running on. A Windows user reading "for macOS"
    /// is reading a port.
    #[test]
    fn the_tagline_names_the_host_platform() {
        assert_eq!(
            AboutContent::for_platform(Platform::MacOs).tagline,
            "A fast image viewer for macOS."
        );
        assert_eq!(
            AboutContent::for_platform(Platform::Windows).tagline,
            "A fast image viewer for Windows."
        );
        assert_eq!(
            AboutContent::for_platform(Platform::Linux).tagline,
            "A fast image viewer for Linux."
        );
    }

    /// The version is the crate's, and it's shown the way macOS has always shown it.
    #[test]
    fn the_version_line_carries_the_crate_version() {
        let content = AboutContent::host();
        assert_eq!(
            content.version,
            format!("Version {}", env!("CARGO_PKG_VERSION"))
        );
    }

    /// The licence sentence reads as one sentence however it's rendered.
    #[test]
    fn the_licence_line_reads_the_same_plain_as_linked() {
        let license = AboutContent::for_platform(Platform::Windows).license;
        assert_eq!(
            license.plain(),
            "Free forever for personal use, under the Business Source License 1.1."
        );
        assert_eq!(
            license.markup(),
            "Free forever for personal use, under the \
             <a href=\"https://github.com/vdavid/prvw/blob/main/LICENSE\">\
             Business Source License 1.1</a>."
        );
    }

    /// `SysLink` reads `&` and `<` as markup, so a label carrying either has to arrive escaped.
    #[test]
    fn markup_escapes_what_syslink_would_otherwise_parse() {
        let line = LicenseLine {
            prefix: "Ampersand & ",
            link: Link {
                label: "a <tag> & more",
                url: "https://example.com/?a=1&b=2",
            },
            suffix: " < end",
        };
        // `>` needs no escaping: `SysLink` only looks for `&` and `<`.
        assert_eq!(
            line.markup(),
            "Ampersand &amp; <a href=\"https://example.com/?a=1&amp;b=2\">\
             a &lt;tag> &amp; more</a> &lt; end"
        );
    }

    /// Every link points somewhere, over https.
    #[test]
    fn every_link_is_an_https_url() {
        let content = AboutContent::host();
        for link in [
            &content.author_site,
            &content.license.link,
            &content.website,
        ] {
            assert!(
                link.url.starts_with("https://"),
                "{} points at {}",
                link.label,
                link.url
            );
            assert!(!link.label.is_empty());
        }
    }

    /// `docs/style-guide.md`, applied to the one milestone that is almost entirely copy. An em
    /// dash is the house's clearest tell, and the three trivializing words are the ones that
    /// creep back in.
    #[test]
    fn the_copy_follows_the_style_guide() {
        for platform in Platform::ALL {
            for line in AboutContent::for_platform(*platform).user_visible_strings() {
                assert!(!line.contains('\u{2014}'), "em dash in {line:?}");
                let lowercase = line.to_lowercase();
                for banned in ["just ", "simply ", "simple ", "easy "] {
                    assert!(!lowercase.contains(banned), "{banned:?} in {line:?}");
                }
            }
        }
    }
}
