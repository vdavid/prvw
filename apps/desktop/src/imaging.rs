//! Image loading, decoding, color management, and navigation.
//!
//! Named `imaging` rather than `image` to avoid clashing with the external `image` crate.
//!
//! - `loader` — format-specific decoders (zune-jpeg for JPEG, `image` crate for others),
//!   ICC profile extraction.
//! - `color` — ICC transform from source profile to target (display or sRGB), using moxcms.
//! - `preloader` — rayon-based background decoding with LRU-bounded `ImageCache` (512 MB).
//! - `directory` — scan parent dir for images, sort, track current position.

pub mod color;
pub mod directory;
pub mod loader;
pub mod preloader;
