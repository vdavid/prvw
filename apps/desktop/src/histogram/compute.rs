//! Histogram counts from a decoded `PixelBuffer`.
//!
//! 256 bins per channel. RGBA8 takes the top byte directly. RGBA16F clamps to
//! [0, 1] and quantizes via `(x * 255).round()`. Above-1.0 EDR samples land in
//! bin 255, which is the right thing for a viewer histogram (they show as
//! clipped highlights).

use crate::decoding::PixelBuffer;
use half::f16;
use rayon::prelude::*;

const BINS: usize = 256;
/// Above this many pixels, split the buffer across rayon threads. Below it
/// the per-iter overhead of par_chunks isn't worth it; a single tight loop
/// is cache-friendlier.
const PARALLEL_THRESHOLD: usize = 4_000_000;

#[derive(Clone, Debug)]
pub struct HistogramData {
    pub r: [u32; BINS],
    pub g: [u32; BINS],
    pub b: [u32; BINS],
    /// Largest single-channel bin count. Drives Y normalization for plotting.
    pub max_count: u32,
}

impl HistogramData {
    fn empty() -> Self {
        Self {
            r: [0; BINS],
            g: [0; BINS],
            b: [0; BINS],
            max_count: 0,
        }
    }

    fn merge(&mut self, other: &Self) {
        for i in 0..BINS {
            self.r[i] += other.r[i];
            self.g[i] += other.g[i];
            self.b[i] += other.b[i];
        }
    }

    fn finalize(&mut self) {
        let mut peak = 0u32;
        for i in 0..BINS {
            peak = peak.max(self.r[i]).max(self.g[i]).max(self.b[i]);
        }
        self.max_count = peak;
    }
}

/// Compute RGB histograms for a decoded image. Single allocation, single
/// pass. Splits across rayon threads for >4 MP buffers.
pub fn compute(pixels: &PixelBuffer) -> HistogramData {
    let mut hist = match pixels {
        PixelBuffer::Rgba8(bytes) => compute_rgba8(bytes),
        PixelBuffer::Rgba16F(halfs) => compute_rgba16f(halfs),
    };
    hist.finalize();
    hist
}

fn compute_rgba8(bytes: &[u8]) -> HistogramData {
    let pixel_count = bytes.len() / 4;
    if pixel_count >= PARALLEL_THRESHOLD {
        // Split into chunks of whole pixels (multiple of 4 bytes).
        let chunk_pixels = pixel_count.div_ceil(rayon::current_num_threads().max(1));
        let chunk_bytes = chunk_pixels * 4;
        bytes
            .par_chunks(chunk_bytes)
            .map(rgba8_chunk)
            .reduce(HistogramData::empty, |mut a, b| {
                a.merge(&b);
                a
            })
    } else {
        rgba8_chunk(bytes)
    }
}

fn rgba8_chunk(bytes: &[u8]) -> HistogramData {
    let mut hist = HistogramData::empty();
    for px in bytes.as_chunks::<4>().0 {
        hist.r[px[0] as usize] += 1;
        hist.g[px[1] as usize] += 1;
        hist.b[px[2] as usize] += 1;
    }
    hist
}

fn compute_rgba16f(halfs: &[u16]) -> HistogramData {
    let pixel_count = halfs.len() / 4;
    if pixel_count >= PARALLEL_THRESHOLD {
        let chunk_pixels = pixel_count.div_ceil(rayon::current_num_threads().max(1));
        let chunk_halfs = chunk_pixels * 4;
        halfs
            .par_chunks(chunk_halfs)
            .map(rgba16f_chunk)
            .reduce(HistogramData::empty, |mut a, b| {
                a.merge(&b);
                a
            })
    } else {
        rgba16f_chunk(halfs)
    }
}

fn rgba16f_chunk(halfs: &[u16]) -> HistogramData {
    let mut hist = HistogramData::empty();
    for px in halfs.as_chunks::<4>().0 {
        hist.r[quantize_half(px[0])] += 1;
        hist.g[quantize_half(px[1])] += 1;
        hist.b[quantize_half(px[2])] += 1;
    }
    hist
}

fn quantize_half(bits: u16) -> usize {
    let v = f16::from_bits(bits).to_f32();
    let clamped = v.clamp(0.0, 1.0);
    (clamped * 255.0).round() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba8_known_pixels() {
        // 4 pixels: (0,0,0), (255,255,255), (128,64,200), (255,0,128).
        let bytes: Vec<u8> = vec![
            0, 0, 0, 255, //
            255, 255, 255, 255, //
            128, 64, 200, 255, //
            255, 0, 128, 255, //
        ];
        let hist = compute(&PixelBuffer::Rgba8(bytes));

        assert_eq!(hist.r[0], 1);
        assert_eq!(hist.r[128], 1);
        assert_eq!(hist.r[255], 2);
        assert_eq!(hist.g[0], 2);
        assert_eq!(hist.g[64], 1);
        assert_eq!(hist.g[255], 1);
        assert_eq!(hist.b[0], 1);
        assert_eq!(hist.b[128], 1);
        assert_eq!(hist.b[200], 1);
        assert_eq!(hist.b[255], 1);
        assert_eq!(hist.max_count, 2);
    }

    #[test]
    fn rgba16f_known_pixels() {
        // 3 pixels: black, white, mid-gray (0.5). Above-1.0 should clamp to bin 255.
        let pixels = [
            (0.0_f32, 0.0_f32, 0.0_f32),
            (1.0, 1.0, 1.0),
            (0.5, 0.5, 0.5),
            (1.5, 0.0, 0.0), // R clipped, G black, B black
        ];
        let mut halfs: Vec<u16> = Vec::with_capacity(pixels.len() * 4);
        for &(r, g, b) in &pixels {
            halfs.push(f16::from_f32(r).to_bits());
            halfs.push(f16::from_f32(g).to_bits());
            halfs.push(f16::from_f32(b).to_bits());
            halfs.push(f16::from_f32(1.0).to_bits());
        }
        let hist = compute(&PixelBuffer::Rgba16F(halfs));

        // 0.5 * 255 = 127.5 → rounds to 128.
        assert_eq!(hist.r[0], 1);
        assert_eq!(hist.r[128], 1);
        // Two reds at 255 (the explicit 1.0 and the clamped 1.5).
        assert_eq!(hist.r[255], 2);
        assert_eq!(hist.g[0], 2);
        assert_eq!(hist.g[128], 1);
        assert_eq!(hist.g[255], 1);
        assert_eq!(hist.b[0], 2);
        assert_eq!(hist.b[128], 1);
        assert_eq!(hist.b[255], 1);
    }

    #[test]
    fn empty_buffer_is_safe() {
        let hist = compute(&PixelBuffer::Rgba8(Vec::new()));
        assert_eq!(hist.max_count, 0);
        assert!(hist.r.iter().all(|&c| c == 0));
    }
}
