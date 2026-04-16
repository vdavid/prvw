//! GPU rendering, view math, and text overlay.
//!
//! - `renderer` — `wgpu` surface, pipelines, texture upload, screenshot path.
//! - `view` — zoom/pan math, transform uniform, `fit_zoom`, cursor-centered zoom.
//! - `text` — text layout + glyph geometry for the overlay pill rendering.
//! - `shader.wgsl` — main image quad shader.
//! - `overlay.wgsl` — pill/text overlay shader.

pub mod renderer;
pub mod text;
pub mod view;
