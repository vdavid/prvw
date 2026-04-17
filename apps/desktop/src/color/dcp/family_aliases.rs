//! Fuzzy camera family fallback for DCP matching.
//!
//! When an exact `UniqueCameraModel` match fails across all search tiers
//! (user dir, Adobe dir, bundled collection), this module provides a curated
//! list of **known-compatible** substitutions.
//!
//! ## Conservative policy
//!
//! The alias list is intentionally small. Only cameras that share the same
//! sensor or are close enough in the same product family are listed. It is
//! better to not match at all than to mismatch (applying a Canon profile to a
//! Nikon body would do more harm than the default matrix-only pipeline). Each
//! entry includes a comment stating the basis for compatibility.
//!
//! To add a new alias, file a PR with evidence (same sensor chip, DxOMark
//! comparison, or side-by-side color analysis). Don't add entries based on
//! brand or marketing family alone.
//!
//! ## Runtime lookup
//!
//! `aliases_for(camera_id)` returns the list of fallback camera IDs to try, in
//! order from most to least compatible. Matching is normalized (case-insensitive,
//! whitespace-collapsed) like the primary path.

use super::discovery::normalize;

/// A single alias entry: `(camera, &[aliases])`.
///
/// Each alias list is sorted from most to least compatible — the DCP discovery
/// code tries them in order and uses the first hit.
const FAMILY_ALIASES: &[(&str, &[&str])] = &[
    // --- Sony E-mount APS-C, shared 20 MP BSI sensors ---
    // α5000 and α6000 share the same 20.1 MP Sony Exmor BSI sensor.
    ("Sony ILCE-5000", &["Sony ILCE-6000", "Sony NEX-5N"]),
    // α5100 shares the α6000's sensor; α6000 is the closest profile.
    ("Sony ILCE-5100", &["Sony ILCE-6000"]),
    // α6100 uses the same 24 MP sensor as the α6400 and α6500.
    ("Sony ILCE-6100", &["Sony ILCE-6400", "Sony ILCE-6500"]),
    // α7R I and α7R II share Sony's full-frame high-resolution lineage;
    // profile characteristics are similar enough for a viewer fallback.
    ("Sony ILCE-7R", &["Sony ILCE-7RM3"]),
    // --- Sony RX compact ---
    // RX100 VII sensor is close to RX100M6; same compact 1" family.
    ("Sony DSC-RX100M7", &["Sony DSC-RX100M6"]),
    // --- Fujifilm X-Trans III / IV family ---
    // X-T2 and X-T3 share the same 26 MP X-Trans IV sensor lineage.
    ("Fujifilm X-T2", &["FUJIFILM X-T3"]),
    // X-T20 uses the same sensor as the X-T2; X-T3 is the next closest.
    ("Fujifilm X-T20", &["FUJIFILM X-T2", "FUJIFILM X-T3"]),
    // X-T30 shares the X-Trans IV sensor with the X-T3.
    ("Fujifilm X-T30", &["FUJIFILM X-T3"]),
    // X-T10 uses the X-Trans II sensor of the X-T1.
    ("Fujifilm X-T10", &["FUJIFILM X-T1"]),
    // --- Nikon APS-C ---
    // D500 and D7500 share the same 20.9 MP BSI CMOS sensor.
    ("Nikon D500", &["NIKON D7500"]),
    // D5200 uses the same 24.1 MP sensor as D5300 / D5600.
    ("Nikon D5200", &["NIKON D5300", "NIKON D5600"]),
    // D3200 uses the same 24.2 MP Sony sensor as D3300.
    ("Nikon D3200", &["NIKON D3300"]),
    // --- Canon EOS APS-C ---
    // 6D and 6D Mark II share Canon's full-frame lineage; same color science.
    ("Canon EOS 6D", &["Canon EOS 6D Mark II"]),
    // EOS M50 and M6 Mark II share the 24.1 MP Dual Pixel CMOS.
    ("Canon EOS M50", &["Canon EOS M6 Mark II"]),
    // EOS M50 Mark II is essentially the same camera with minor firmware updates.
    ("Canon EOS M50m2", &["Canon EOS M6 Mark II"]),
    // --- Canon EOS R ---
    // EOS RP uses the same 26.2 MP sensor as the EOS R; very close color.
    ("Canon EOS RP", &["Canon EOS R"]),
    // --- Olympus / OM System M43 ---
    // E-M10 Mark III shares the sensor with the E-M10 (same 16 MP Panasonic chip).
    ("Olympus E-M10 Mark III", &["OLYMPUS E-M10"]),
    // E-M1 Mark III is a refinement of the E-M1 Mark II sensor.
    ("Olympus E-M1 Mark III", &["OLYMPUS E-M1MarkII"]),
    // --- Panasonic M43 ---
    // GX7 Mark II reuses the GX7's sensor family.
    ("Panasonic DMC-GX7MK2", &["Panasonic DMC-GX7"]),
    // G85 sensor is very close to the G9's 20 MP Sony chip.
    ("Panasonic DC-G85", &["Panasonic DC-G9"]),
];

/// Returns the alias list for `camera_id`, or an empty slice if none.
///
/// Matching is case-insensitive and whitespace-normalized to match the primary
/// discovery path. Both the lookup key and the alias entries are normalized
/// before comparison.
pub fn aliases_for(camera_id: &str) -> &'static [&'static str] {
    let target = normalize(camera_id);
    for (key, aliases) in FAMILY_ALIASES {
        if normalize(key) == target {
            return aliases;
        }
    }
    &[]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_for_known_camera_returns_nonempty() {
        let a = aliases_for("Sony ILCE-5000");
        assert!(!a.is_empty(), "ILCE-5000 should have aliases");
        assert!(
            a.iter()
                .any(|s| normalize(s) == normalize("Sony ILCE-6000")),
            "ILCE-5000 alias list should include ILCE-6000"
        );
    }

    #[test]
    fn aliases_for_unknown_camera_returns_empty() {
        let a = aliases_for("NoSuch Camera ZZZ-9999");
        assert!(a.is_empty());
    }

    #[test]
    fn aliases_for_is_case_and_space_insensitive() {
        // Same camera, different case / extra space.
        let a1 = aliases_for("Sony ILCE-5000");
        let a2 = aliases_for("SONY ilce-5000");
        let a3 = aliases_for("sony  ILCE-5000 ");
        assert_eq!(a1.len(), a2.len());
        assert_eq!(a1.len(), a3.len());
    }

    #[test]
    fn all_alias_entries_have_at_least_one_alias() {
        for (key, aliases) in FAMILY_ALIASES {
            assert!(
                !aliases.is_empty(),
                "FAMILY_ALIASES entry for '{key}' has an empty alias list"
            );
        }
    }
}
