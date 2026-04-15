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

impl Logical<f32> {
    pub fn to_physical(self, scale_factor: f64) -> Physical<f32> {
        Physical(self.0 * scale_factor as f32)
    }
}

impl Logical<f64> {
    pub fn to_physical(self, scale_factor: f64) -> Physical<f64> {
        Physical(self.0 * scale_factor)
    }

    pub fn as_f32(self) -> Logical<f32> {
        Logical(self.0 as f32)
    }
}

impl Physical<f32> {
    pub fn to_logical(self, scale_factor: f64) -> Logical<f32> {
        Logical(self.0 / scale_factor as f32)
    }
}

impl Physical<f64> {
    pub fn to_logical(self, scale_factor: f64) -> Logical<f64> {
        Logical(self.0 / scale_factor)
    }
}

impl Physical<u32> {
    pub fn to_logical_f32(self, scale_factor: f64) -> Logical<f32> {
        Logical(self.0 as f32 / scale_factor as f32)
    }
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
