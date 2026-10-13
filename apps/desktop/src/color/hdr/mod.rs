//! HDR stills beyond RAW: gain-map JPEGs, PQ/HLG PNGs, and float TIFFs. The plan is
//! `docs/specs/hdr-images.md`.
//!
//! `ultrahdr-core` is pre-1.0 and renames things between minor versions, so the rest of the app
//! reaches it only through this module, and a bump touches one place. It's added with
//! `default-features = false` on purpose: see the comment on it in `apps/desktop/Cargo.toml`.

#[cfg(test)]
mod tests {
    use ultrahdr_core::GainMapMetadata;
    use ultrahdr_core::gainmap::apply::calculate_weight;

    fn metadata(base_headroom: f64, alternate_headroom: f64) -> GainMapMetadata {
        let mut m = GainMapMetadata::default();
        m.base_hdr_headroom = base_headroom;
        m.alternate_hdr_headroom = alternate_headroom;
        m
    }

    /// The usual direction (SDR base, HDR alternate) is the one `calculate_weight` gets right,
    /// so our shader port can be tested against it there.
    #[test]
    fn upstream_weight_matches_the_formula_for_an_sdr_base() {
        let m = metadata(0.0, 2.0);
        assert_eq!(calculate_weight(1.0, &m), 0.0);
        assert_eq!(calculate_weight(2.0, &m), 0.5);
        assert_eq!(calculate_weight(4.0, &m), 1.0);
        assert_eq!(calculate_weight(16.0, &m), 1.0);
    }

    /// Regression anchor for a finding in the spec (2026-10-13): with an HDR base (ISO 21496-1
    /// allows it), `ultrahdr-core` 0.6.0 returns a weight of 1.0 at every headroom, which would
    /// show the SDR rendition even on a display that can show the HDR base as is. The correct
    /// weight there runs from 1.0 at headroom 1 down to 0.0 at the base's own headroom, so the
    /// gain-map port takes libavif's formula for that direction. If an upgrade makes this test
    /// fail, upstream changed the behavior: re-check the port against it.
    #[test]
    fn upstream_weight_ignores_an_hdr_base() {
        let m = metadata(2.0, 0.0);
        for headroom in [1.0, 2.0, 4.0, 16.0] {
            assert_eq!(calculate_weight(headroom, &m), 1.0);
        }
    }
}
