//! Newtype wrappers for pixel coordinates. The compiler prevents mixing logical and physical values.
//!
//! - **Logical**: display-independent points. 1 logical = `scale_factor` physical. Used for
//!   window positions, UI layout, zoom model, text coordinates, and all user-facing values.
//! - **Physical**: actual GPU surface pixels. Used only inside the renderer for wgpu surfaces.

use std::fmt;
use std::ops::{Add, Div, Mul, Neg, Sub};

/// A value in logical pixels (display-independent points).
#[derive(Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Logical<T>(pub T);

/// A value in physical pixels (GPU surface pixels).
#[derive(Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Physical<T>(pub T);

// ── Conversion ───────────────────────────────────────────────────────────

impl Logical<f64> {
    pub fn as_f32(self) -> Logical<f32> {
        Logical(self.0 as f32)
    }
}

impl Physical<u32> {
    pub fn to_logical_f32(self, scale_factor: f64) -> Logical<f32> {
        Logical(self.0 as f32 / scale_factor as f32)
    }
}

// ── Winit interop ────────────────────────────────────────────────────────

use winit::dpi;

/// Extract logical width and height from a winit `LogicalSize`.
pub fn from_logical_size(s: dpi::LogicalSize<f64>) -> (Logical<f64>, Logical<f64>) {
    (Logical(s.width), Logical(s.height))
}

/// Extract logical x and y from a winit `LogicalPosition`.
pub fn from_logical_pos(p: dpi::LogicalPosition<f64>) -> (Logical<f64>, Logical<f64>) {
    (Logical(p.x), Logical(p.y))
}

/// Extract physical width and height from a winit `PhysicalSize`.
pub fn from_physical_size(s: dpi::PhysicalSize<u32>) -> (Physical<u32>, Physical<u32>) {
    (Physical(s.width), Physical(s.height))
}

/// Create a winit `LogicalSize` from logical width and height.
pub fn to_logical_size(w: Logical<f64>, h: Logical<f64>) -> dpi::LogicalSize<f64> {
    dpi::LogicalSize::new(w.0, h.0)
}

/// Create a winit `LogicalPosition` from logical x and y.
pub fn to_logical_pos(x: Logical<f64>, y: Logical<f64>) -> dpi::LogicalPosition<f64> {
    dpi::LogicalPosition::new(x.0, y.0)
}

// ── Debug / Display ──────────────────────────────────────────────────────

impl<T: fmt::Debug> fmt::Debug for Logical<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "L({:?})", self.0)
    }
}

impl<T: fmt::Debug> fmt::Debug for Physical<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "P({:?})", self.0)
    }
}

impl<T: fmt::Display> fmt::Display for Logical<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: fmt::Display> fmt::Display for Physical<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

// ── Arithmetic for Logical<f32> ──────────────────────────────────────────

impl Add for Logical<f32> {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl Sub for Logical<f32> {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl Mul<f32> for Logical<f32> {
    type Output = Self;
    fn mul(self, rhs: f32) -> Self {
        Self(self.0 * rhs)
    }
}

impl Div<f32> for Logical<f32> {
    type Output = Self;
    fn div(self, rhs: f32) -> Self {
        Self(self.0 / rhs)
    }
}

impl Div for Logical<f32> {
    type Output = f32;
    fn div(self, rhs: Self) -> f32 {
        self.0 / rhs.0
    }
}

impl Neg for Logical<f32> {
    type Output = Self;
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

// ── Arithmetic for Logical<f64> ──────────────────────────────────────────

impl Add for Logical<f64> {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl Sub for Logical<f64> {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl Mul<f64> for Logical<f64> {
    type Output = Self;
    fn mul(self, rhs: f64) -> Self {
        Self(self.0 * rhs)
    }
}

impl Div<f64> for Logical<f64> {
    type Output = Self;
    fn div(self, rhs: f64) -> Self {
        Self(self.0 / rhs)
    }
}

impl Div for Logical<f64> {
    type Output = f64;
    fn div(self, rhs: Self) -> f64 {
        self.0 / rhs.0
    }
}

impl Neg for Logical<f64> {
    type Output = Self;
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

// ── Arithmetic for Physical<u32> ─────────────────────────────────────────

impl Add for Physical<u32> {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl Sub for Physical<u32> {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}
