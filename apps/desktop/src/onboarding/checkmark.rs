//! Render the onboarding checkmark glyph as an `NSImage` at the requested size.
//!
//! We carry a single hand-picked SVG path (see `CHECKMARK_PATH_D`). The renderer
//! parses just the path commands we need (`M`, `c`, `s`, `q`, `a`, `l`), converts
//! each to `NSBezierPath` operations, and fills it into a focus-locked `NSImage`.
//! Keeping the parser tight to what our one path uses avoids pulling in a full SVG
//! crate for a single icon.
//!
//! The image renders in two variants:
//! - `Green`: solid `#189d34` — the "checked" state.
//! - `Dim`: same path at 15% alpha of the macOS `labelColor` — the "unchecked"
//!   state, so the placeholder reads in both light and dark appearance.
//!
//! Math notes: elliptical arcs use the endpoint→center conversion from the SVG
//! 1.1 implementation notes (appendix F.6), then approximate each arc with cubic
//! Béziers per segment (capped at 90°). Cubic/quadratic curves translate directly;
//! the `s` shorthand reflects the previous cubic's second control point.

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_app_kit::{NSBezierPath, NSColor, NSImage};
use objc2_foundation::{NSPoint, NSSize};

/// SVG path data for the checkmark. Taken verbatim from the onboarding spec.
const CHECKMARK_PATH_D: &str = "M335 98.2c8.5-.4 16.2 8.5 15.4 16.8-.3 2.7-2 5.4-3.9 7.4-4 4.4-8.8 7.8-13.3 11.8q-11.6 10.4-22.4 21.8a583 583 0 0 0-82.1 104.4c-13.2 22-25.2 45-34.3 69q-4.4 11.6-8 23.4c-1.4 4.7-2.2 10.3-5.6 14s-10.1 3.2-14.8 3.3q-11.3.5-22.3 2.1c-4.3.8-8.6 1.8-13 1.9-5.5.1-8.2-3.1-11.2-7.3-4.7-6.6-8.7-13.7-13.1-20.4a351 351 0 0 0-59.7-67.6c-3.5-2.8-7.3-5.8-10-9.4-4-5.4-.3-12.3 3.3-16.6 4.8-5.7 10.8-10.1 16.8-14.6 6.6-4.8 13.7-10.5 21.3-13.7a33 33 0 0 1 25.1.6c7.3 3.6 12.3 11 17 17.4a334 334 0 0 1 20.8 33.9l4.2 8.1c.5.6 1.2 2.6 2 2.2 1.2-.6 2-2.9 2.6-3.9q3.3-5.1 6.4-10.6a442 442 0 0 1 23.3-36 555 555 0 0 1 149.2-135.7c2-1.1 4-2.3 6.3-2.3";

/// The viewBox from the source SVG. We scale the parsed path into the requested
/// image size while preserving aspect.
const VIEWBOX_WIDTH: f64 = 376.0;
const VIEWBOX_HEIGHT: f64 = 396.0;

/// Rendering variant for the checkmark.
#[derive(Copy, Clone, Debug)]
pub(super) enum CheckState {
    /// Green (#189d34) — step is complete.
    Green,
    /// Muted gray/label color at 15% alpha — step is pending.
    Dim,
}

/// Render the checkmark into a square `NSImage` of side `size_pt` (points).
#[allow(deprecated)] // lockFocus/unlockFocus are deprecated in favor of a block-based API;
// we use the simpler legacy path since the image is small and static.
pub(super) fn make_image(size_pt: f64, state: CheckState) -> Retained<NSImage> {
    let image_size = NSSize::new(size_pt, size_pt);
    let image = NSImage::initWithSize(NSImage::alloc(), image_size);

    let path = build_bezier_path(size_pt);

    // `lockFocus` uses the image's default flipped-ness (false). Y axis flipping
    // is already baked into `build_bezier_path`, so the glyph draws upright.
    image.lockFocus();
    let color = match state {
        CheckState::Green => checkmark_green(),
        CheckState::Dim => dim_label_color(),
    };
    color.set();
    path.fill();
    image.unlockFocus();

    image
}

fn checkmark_green() -> Retained<NSColor> {
    // #189d34 in sRGB, fully opaque.
    NSColor::colorWithSRGBRed_green_blue_alpha(
        0x18 as f64 / 255.0,
        0x9d as f64 / 255.0,
        0x34 as f64 / 255.0,
        1.0,
    )
}

fn dim_label_color() -> Retained<NSColor> {
    // `labelColor` at 15% alpha — reads as a soft placeholder in both light and
    // dark appearance.
    let base: Retained<NSColor> = NSColor::labelColor();
    base.colorWithAlphaComponent(0.15)
}

/// Parse the SVG path and produce an `NSBezierPath` sized to fit inside an
/// `image_side × image_side` square. Applies uniform scaling with letterboxing
/// so the aspect ratio of the source viewBox is preserved and the glyph is
/// centered.
fn build_bezier_path(image_side: f64) -> Retained<NSBezierPath> {
    let path = NSBezierPath::bezierPath();

    let scale = (image_side / VIEWBOX_WIDTH).min(image_side / VIEWBOX_HEIGHT);
    let scaled_w = VIEWBOX_WIDTH * scale;
    let scaled_h = VIEWBOX_HEIGHT * scale;
    let offset_x = (image_side - scaled_w) / 2.0;
    let offset_y = (image_side - scaled_h) / 2.0;

    // SVG Y grows downward; AppKit default Y grows upward. Flip by subtracting
    // from the scaled height.
    let project = |x: f64, y: f64| -> NSPoint {
        NSPoint::new(
            offset_x + x * scale,
            offset_y + (VIEWBOX_HEIGHT - y) * scale,
        )
    };

    let commands = parse_path(CHECKMARK_PATH_D);

    // Track the cursor (last on-curve point) and the previous cubic's second
    // control point — needed for the `s` shorthand.
    let mut cursor = (0.0_f64, 0.0_f64);
    let mut last_cubic_c2: Option<(f64, f64)> = None;
    let mut subpath_start = cursor;

    for cmd in &commands {
        match cmd {
            PathCmd::MoveAbs(x, y) => {
                cursor = (*x, *y);
                subpath_start = cursor;
                path.moveToPoint(project(cursor.0, cursor.1));
                last_cubic_c2 = None;
            }
            PathCmd::CubicRel { c1, c2, end } => {
                let c1_abs = (cursor.0 + c1.0, cursor.1 + c1.1);
                let c2_abs = (cursor.0 + c2.0, cursor.1 + c2.1);
                let end_abs = (cursor.0 + end.0, cursor.1 + end.1);
                path.curveToPoint_controlPoint1_controlPoint2(
                    project(end_abs.0, end_abs.1),
                    project(c1_abs.0, c1_abs.1),
                    project(c2_abs.0, c2_abs.1),
                );
                cursor = end_abs;
                last_cubic_c2 = Some(c2_abs);
            }
            PathCmd::SmoothCubicRel { c2, end } => {
                // Reflect the previous cubic's c2 across the cursor to get c1.
                // If no previous cubic, c1 = cursor.
                let c1_abs = match last_cubic_c2 {
                    Some(prev_c2) => (2.0 * cursor.0 - prev_c2.0, 2.0 * cursor.1 - prev_c2.1),
                    None => cursor,
                };
                let c2_abs = (cursor.0 + c2.0, cursor.1 + c2.1);
                let end_abs = (cursor.0 + end.0, cursor.1 + end.1);
                path.curveToPoint_controlPoint1_controlPoint2(
                    project(end_abs.0, end_abs.1),
                    project(c1_abs.0, c1_abs.1),
                    project(c2_abs.0, c2_abs.1),
                );
                cursor = end_abs;
                last_cubic_c2 = Some(c2_abs);
            }
            PathCmd::QuadraticRel { ctrl, end } => {
                let ctrl_abs = (cursor.0 + ctrl.0, cursor.1 + ctrl.1);
                let end_abs = (cursor.0 + end.0, cursor.1 + end.1);
                path.curveToPoint_controlPoint(
                    project(end_abs.0, end_abs.1),
                    project(ctrl_abs.0, ctrl_abs.1),
                );
                cursor = end_abs;
                last_cubic_c2 = None;
            }
            PathCmd::LineRel(dx, dy) => {
                cursor = (cursor.0 + dx, cursor.1 + dy);
                path.lineToPoint(project(cursor.0, cursor.1));
                last_cubic_c2 = None;
            }
            PathCmd::ArcRel {
                rx,
                ry,
                x_axis_rotation_deg,
                large_arc,
                sweep,
                end,
            } => {
                let end_abs = (cursor.0 + end.0, cursor.1 + end.1);
                for (c1, c2, ep) in arc_to_cubics(
                    cursor,
                    end_abs,
                    *rx,
                    *ry,
                    *x_axis_rotation_deg,
                    *large_arc,
                    *sweep,
                ) {
                    path.curveToPoint_controlPoint1_controlPoint2(
                        project(ep.0, ep.1),
                        project(c1.0, c1.1),
                        project(c2.0, c2.1),
                    );
                }
                cursor = end_abs;
                last_cubic_c2 = None;
            }
            PathCmd::Close => {
                path.closePath();
                cursor = subpath_start;
                last_cubic_c2 = None;
            }
        }
    }

    // Close the fill region so macOS's fill operator renders the glyph as a
    // filled shape. Our source path omits `Z`, but the glyph is a closed silhouette.
    path.closePath();

    path
}

#[derive(Debug, Clone)]
enum PathCmd {
    MoveAbs(f64, f64),
    CubicRel {
        c1: (f64, f64),
        c2: (f64, f64),
        end: (f64, f64),
    },
    SmoothCubicRel {
        c2: (f64, f64),
        end: (f64, f64),
    },
    QuadraticRel {
        ctrl: (f64, f64),
        end: (f64, f64),
    },
    LineRel(f64, f64),
    ArcRel {
        rx: f64,
        ry: f64,
        x_axis_rotation_deg: f64,
        large_arc: bool,
        sweep: bool,
        end: (f64, f64),
    },
    Close,
}

/// Parse the subset of SVG path commands our single checkmark uses.
///
/// Supported: `M` (absolute moveto), `c` (relative cubic), `s` (relative
/// smooth cubic), `q` (relative quadratic), `l` (relative lineto), `a`
/// (relative elliptical arc), `Z`/`z` (close).
///
/// Commands repeat with implicit command letter, so `c a,b,c,d,e,f a,b,c,d,e,f`
/// is two cubic segments. After an `M`, the implicit command becomes `l`
/// (per SVG spec), but our path doesn't exercise that case.
fn parse_path(d: &str) -> Vec<PathCmd> {
    let mut out = Vec::new();
    let mut tokens = Tokens::new(d);

    while let Some(head) = tokens.next_command_or_number() {
        match head {
            Token::Command(c) => parse_one_command(c, &mut tokens, &mut out),
            Token::Number(n) => {
                // Implicit repeat — push the number back and repeat the last command.
                tokens.push_back_number(n);
                if let Some(last) = last_command_letter(&out) {
                    parse_one_command(last, &mut tokens, &mut out);
                } else {
                    break;
                }
            }
        }
    }
    out
}

fn last_command_letter(cmds: &[PathCmd]) -> Option<char> {
    cmds.last().map(|c| match c {
        PathCmd::MoveAbs(..) => 'M',
        PathCmd::CubicRel { .. } => 'c',
        PathCmd::SmoothCubicRel { .. } => 's',
        PathCmd::QuadraticRel { .. } => 'q',
        PathCmd::LineRel(..) => 'l',
        PathCmd::ArcRel { .. } => 'a',
        PathCmd::Close => 'Z',
    })
}

fn parse_one_command(cmd: char, tokens: &mut Tokens, out: &mut Vec<PathCmd>) {
    match cmd {
        'M' => {
            let x = tokens.need_number();
            let y = tokens.need_number();
            out.push(PathCmd::MoveAbs(x, y));
        }
        'c' => {
            let c1 = (tokens.need_number(), tokens.need_number());
            let c2 = (tokens.need_number(), tokens.need_number());
            let end = (tokens.need_number(), tokens.need_number());
            out.push(PathCmd::CubicRel { c1, c2, end });
        }
        's' => {
            let c2 = (tokens.need_number(), tokens.need_number());
            let end = (tokens.need_number(), tokens.need_number());
            out.push(PathCmd::SmoothCubicRel { c2, end });
        }
        'q' => {
            let ctrl = (tokens.need_number(), tokens.need_number());
            let end = (tokens.need_number(), tokens.need_number());
            out.push(PathCmd::QuadraticRel { ctrl, end });
        }
        'l' => {
            let dx = tokens.need_number();
            let dy = tokens.need_number();
            out.push(PathCmd::LineRel(dx, dy));
        }
        'a' => {
            let rx = tokens.need_number();
            let ry = tokens.need_number();
            let rot = tokens.need_number();
            let large = tokens.need_flag();
            let sweep = tokens.need_flag();
            let end = (tokens.need_number(), tokens.need_number());
            out.push(PathCmd::ArcRel {
                rx,
                ry,
                x_axis_rotation_deg: rot,
                large_arc: large,
                sweep,
                end,
            });
        }
        'Z' | 'z' => out.push(PathCmd::Close),
        other => {
            log::warn!("Checkmark SVG parser: unsupported command '{other}'");
        }
    }
}

#[derive(Debug)]
enum Token {
    Command(char),
    Number(f64),
}

/// Tiny streaming tokenizer for SVG path data. Numbers are plain decimals; no
/// scientific notation is needed for our path, but we still accept it. Flags
/// (used only by `a`/`A`) are single digits 0 or 1.
struct Tokens<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// When non-empty, `next_command_or_number` returns these buffered numbers
    /// before reading more input. We only ever push one back (for implicit
    /// command repetition), so a small inline buffer would suffice — `Vec`
    /// keeps the code boring.
    pushed_back: Vec<f64>,
}

impl<'a> Tokens<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            bytes: s.as_bytes(),
            pos: 0,
            pushed_back: Vec::new(),
        }
    }

    fn skip_whitespace_and_commas(&mut self) {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == b',' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn next_command_or_number(&mut self) -> Option<Token> {
        if let Some(n) = self.pushed_back.pop() {
            return Some(Token::Number(n));
        }
        self.skip_whitespace_and_commas();
        if self.pos >= self.bytes.len() {
            return None;
        }
        let b = self.bytes[self.pos];
        if b.is_ascii_alphabetic() {
            self.pos += 1;
            Some(Token::Command(b as char))
        } else {
            Some(Token::Number(self.read_number()))
        }
    }

    fn need_number(&mut self) -> f64 {
        if let Some(n) = self.pushed_back.pop() {
            return n;
        }
        self.skip_whitespace_and_commas();
        self.read_number()
    }

    fn need_flag(&mut self) -> bool {
        self.skip_whitespace_and_commas();
        if self.pos >= self.bytes.len() {
            return false;
        }
        let b = self.bytes[self.pos];
        self.pos += 1;
        b == b'1'
    }

    fn read_number(&mut self) -> f64 {
        let start = self.pos;
        // Optional sign
        if self.pos < self.bytes.len()
            && (self.bytes[self.pos] == b'-' || self.bytes[self.pos] == b'+')
        {
            self.pos += 1;
        }
        // Digits before decimal
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        // Decimal
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'.' {
            self.pos += 1;
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        // Exponent
        if self.pos < self.bytes.len()
            && (self.bytes[self.pos] == b'e' || self.bytes[self.pos] == b'E')
        {
            self.pos += 1;
            if self.pos < self.bytes.len()
                && (self.bytes[self.pos] == b'-' || self.bytes[self.pos] == b'+')
            {
                self.pos += 1;
            }
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        let slice = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap_or("0");
        slice.parse::<f64>().unwrap_or(0.0)
    }
}

impl<'a> Tokens<'a> {
    fn push_back_number(&mut self, n: f64) {
        self.pushed_back.push(n);
    }
}

// ─── Arc-to-cubic conversion ────────────────────────────────────────────
//
// From the SVG 1.1 Implementation Notes (F.6), plus the standard cubic Bézier
// approximation of a circular arc segment (kappa per ≤90° step).

type Point = (f64, f64);
/// A single cubic Bézier segment: `(control1, control2, end_point)`.
type CubicSeg = (Point, Point, Point);

fn arc_to_cubics(
    start: Point,
    end: Point,
    rx_in: f64,
    ry_in: f64,
    x_rot_deg: f64,
    large_arc: bool,
    sweep: bool,
) -> Vec<CubicSeg> {
    // Handle degenerate cases: same start/end or zero radius → no geometry.
    if (start.0 - end.0).abs() < f64::EPSILON && (start.1 - end.1).abs() < f64::EPSILON {
        return Vec::new();
    }
    if rx_in.abs() < f64::EPSILON || ry_in.abs() < f64::EPSILON {
        return vec![(start, end, end)];
    }

    let mut rx = rx_in.abs();
    let mut ry = ry_in.abs();

    let phi = x_rot_deg.to_radians();
    let (sin_phi, cos_phi) = phi.sin_cos();

    // Step 1: translate so the midpoint of start→end is at the origin, then
    // rotate by -phi.
    let dx2 = (start.0 - end.0) / 2.0;
    let dy2 = (start.1 - end.1) / 2.0;

    let x1p = cos_phi * dx2 + sin_phi * dy2;
    let y1p = -sin_phi * dx2 + cos_phi * dy2;

    // Step 2: ensure radii are large enough.
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }

    // Step 3: center in the rotated coordinate space.
    let rx_sq = rx * rx;
    let ry_sq = ry * ry;
    let x1p_sq = x1p * x1p;
    let y1p_sq = y1p * y1p;

    let denom = rx_sq * y1p_sq + ry_sq * x1p_sq;
    let numer = rx_sq * ry_sq - rx_sq * y1p_sq - ry_sq * x1p_sq;
    let mut factor = (numer / denom).max(0.0).sqrt();
    if large_arc == sweep {
        factor = -factor;
    }
    let cxp = factor * rx * y1p / ry;
    let cyp = factor * -ry * x1p / rx;

    // Step 4: un-rotate back.
    let cx = cos_phi * cxp - sin_phi * cyp + (start.0 + end.0) / 2.0;
    let cy = sin_phi * cxp + cos_phi * cyp + (start.1 + end.1) / 2.0;

    // Step 5: angles.
    let start_angle = angle_between((1.0, 0.0), ((x1p - cxp) / rx, (y1p - cyp) / ry));
    let mut sweep_angle = angle_between(
        ((x1p - cxp) / rx, (y1p - cyp) / ry),
        ((-x1p - cxp) / rx, (-y1p - cyp) / ry),
    );

    if !sweep && sweep_angle > 0.0 {
        sweep_angle -= 2.0 * std::f64::consts::PI;
    } else if sweep && sweep_angle < 0.0 {
        sweep_angle += 2.0 * std::f64::consts::PI;
    }

    // Split the sweep into ≤90° segments for a tight cubic Bézier approximation.
    let segment_count = (sweep_angle.abs() / (std::f64::consts::FRAC_PI_2)).ceil() as usize;
    let segment_count = segment_count.max(1);
    let delta = sweep_angle / segment_count as f64;

    let mut curves = Vec::with_capacity(segment_count);
    let mut theta1 = start_angle;

    // Control-point length factor for a circular arc of half-angle delta/2:
    //   t = (4/3) * tan(delta/4)
    // (Standard cubic Bézier approximation of a circular arc; error bounded.)
    let ellipse = Ellipse {
        center: (cx, cy),
        radii: (rx, ry),
        rotation: (cos_phi, sin_phi),
    };
    for _ in 0..segment_count {
        let theta2 = theta1 + delta;
        let t = (4.0 / 3.0) * (delta / 4.0).tan();

        let p1 = ellipse.point_at(theta1);
        let p2 = ellipse.point_at(theta2);
        // Derivatives at theta1 and theta2 give the tangent directions.
        let d1 = ellipse.tangent_at(theta1);
        let d2 = ellipse.tangent_at(theta2);

        let c1 = (p1.0 + t * d1.0, p1.1 + t * d1.1);
        let c2 = (p2.0 - t * d2.0, p2.1 - t * d2.1);

        curves.push((c1, c2, p2));
        theta1 = theta2;
    }

    curves
}

/// Parameterized ellipse used to evaluate points and tangents during the arc
/// approximation. Grouping the four parameters into one struct keeps the
/// per-segment call sites short and passes clippy's arg-count lint.
struct Ellipse {
    center: Point,
    radii: Point,
    /// (cos φ, sin φ) for the x-axis rotation — precomputed once per arc.
    rotation: Point,
}

impl Ellipse {
    fn point_at(&self, theta: f64) -> Point {
        let (sin_t, cos_t) = theta.sin_cos();
        let (cos_phi, sin_phi) = self.rotation;
        let (rx, ry) = self.radii;
        let (cx, cy) = self.center;
        let x = cos_phi * rx * cos_t - sin_phi * ry * sin_t + cx;
        let y = sin_phi * rx * cos_t + cos_phi * ry * sin_t + cy;
        (x, y)
    }

    fn tangent_at(&self, theta: f64) -> Point {
        let (sin_t, cos_t) = theta.sin_cos();
        let (cos_phi, sin_phi) = self.rotation;
        let (rx, ry) = self.radii;
        let dx = -cos_phi * rx * sin_t - sin_phi * ry * cos_t;
        let dy = -sin_phi * rx * sin_t + cos_phi * ry * cos_t;
        (dx, dy)
    }
}

fn angle_between(u: Point, v: Point) -> f64 {
    let dot = u.0 * v.0 + u.1 * v.1;
    let len_u = (u.0 * u.0 + u.1 * u.1).sqrt();
    let len_v = (v.0 * v.0 + v.1 * v.1).sqrt();
    let cos_a = (dot / (len_u * len_v)).clamp(-1.0, 1.0);
    let sign = if u.0 * v.1 - u.1 * v.0 >= 0.0 {
        1.0
    } else {
        -1.0
    };
    sign * cos_a.acos()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_reads_full_checkmark_path_without_panic() {
        let cmds = parse_path(CHECKMARK_PATH_D);
        // The real path has at least a MoveAbs plus many curves; sanity check
        // we got a reasonable number of commands.
        assert!(
            cmds.len() > 15,
            "expected >15 parsed commands, got {}",
            cmds.len()
        );
        assert!(matches!(cmds[0], PathCmd::MoveAbs(_, _)));
    }

    #[test]
    fn parser_handles_implicit_command_repeat() {
        // After one `c` command, more 6-tuples of numbers repeat as cubics.
        let cmds = parse_path("M0 0 c1 2 3 4 5 6 7 8 9 10 11 12");
        assert_eq!(cmds.len(), 3);
        assert!(matches!(cmds[0], PathCmd::MoveAbs(_, _)));
        assert!(matches!(cmds[1], PathCmd::CubicRel { .. }));
        assert!(matches!(cmds[2], PathCmd::CubicRel { .. }));
    }

    #[test]
    fn parser_handles_negative_numbers_without_separator() {
        // SVG lets you write "-.4" directly after another number with no comma.
        let cmds = parse_path("M335 98.2c8.5-.4 16.2 8.5 15.4 16.8");
        assert_eq!(cmds.len(), 2);
        if let PathCmd::CubicRel { c1, .. } = &cmds[1] {
            assert!((c1.0 - 8.5).abs() < 1e-9);
            assert!((c1.1 + 0.4).abs() < 1e-9);
        } else {
            panic!("expected cubic, got {:?}", cmds[1]);
        }
    }

    #[test]
    fn arc_to_cubics_quarter_circle_produces_one_segment() {
        // Quarter circle from (1,0) to (0,1) on unit circle.
        let curves = arc_to_cubics((1.0, 0.0), (0.0, 1.0), 1.0, 1.0, 0.0, false, true);
        assert!(!curves.is_empty(), "expected at least one cubic");
        let (_, _, last) = curves.last().unwrap();
        assert!((last.0 - 0.0).abs() < 1e-6);
        assert!((last.1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn arc_to_cubics_splits_large_sweep() {
        // Semicircle sweeps 180°, so we need at least two segments for a ≤90°
        // approximation.
        let curves = arc_to_cubics((1.0, 0.0), (-1.0, 0.0), 1.0, 1.0, 0.0, false, true);
        assert!(curves.len() >= 2);
    }
}
