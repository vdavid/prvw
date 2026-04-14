//! Pixel coordinate type aliases and conversion helpers.
//! These don't prevent mixing at compile time, but make the intent clear at each call site.
//!
//! - **Logical pixels**: display-independent points. 1 logical = `scale_factor` physical.
//! - **Physical pixels**: actual GPU surface pixels.

/// Logical pixels as f64 (window positioning, monitor bounds).
pub type LogicalF64 = f64;

/// Logical pixels as f32 (text/overlay coordinates, renderer convenience methods).
pub type LogicalF32 = f32;

/// Convert logical pixels to physical.
#[expect(
    dead_code,
    reason = "conversion helper, will be used as more code adopts typed pixel aliases"
)]
pub fn to_physical(logical: LogicalF64, scale_factor: f64) -> f64 {
    logical * scale_factor
}

/// Convert physical pixels to logical.
#[expect(
    dead_code,
    reason = "conversion helper, will be used as more code adopts typed pixel aliases"
)]
pub fn to_logical(physical: f64, scale_factor: f64) -> LogicalF64 {
    physical / scale_factor
}
