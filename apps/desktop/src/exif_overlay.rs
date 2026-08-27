//! EXIF info overlay: a toggleable reading panel listing the current
//! image's camera, lens, exposure, time, and GPS metadata. Sits below the
//! histogram (or in its place when the histogram is hidden), top-right.
//!
//! - `State` holds visibility (driven by `Settings::exif_visible`).
//! - `overlay::build` produces the visual layers: a backdrop pill, an
//!   "Exif info" title row, and one label+value pair per data row. Every
//!   row uses the same line pitch — section grouping is implicit in the
//!   label names, not in vertical spacing.
//! - An image with no EXIF gets the panel too, carrying one line that says so. The panel stays
//!   away only while the current image isn't decoded yet, which `App::current_exif_state` is
//!   what tells apart.

pub mod overlay;

use crate::settings::Settings;

pub struct State {
    pub visible: bool,
}

impl State {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            visible: settings.exif_visible,
        }
    }
}
