// Procedural artwork for the demo: avatars, photos, waveforms and the
// desktop backdrop. Everything is generated from a seed, so no real
// picture ever ends up in a screenshot.
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

pub struct Canvas {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Canvas { w, h, px: vec![0; (w * h * 4) as usize] }
    }

    fn put(&mut self, x: u32, y: u32, rgb: [f32; 3], alpha: f32) {
        let i = ((y * self.w + x) * 4) as usize;
        let a = alpha.clamp(0.0, 1.0);
        let dst_a = self.px[i + 3] as f32 / 255.0;
        let out_a = a + dst_a * (1.0 - a);
        if out_a <= 0.0 {
            return;
        }
        for (channel, value) in self.px[i..i + 3].iter_mut().zip(rgb) {
            let dst = *channel as f32 / 255.0;
            let v = (value * a + dst * dst_a * (1.0 - a)) / out_a;
            *channel = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        self.px[i + 3] = (out_a * 255.0).round() as u8;
    }

    pub fn image(&self) -> Image {
        let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(self.w, self.h);
        buf.make_mut_bytes().copy_from_slice(&self.px);
        Image::from_rgba8(buf)
    }
}

fn hash(seed: u32) -> u32 {
    let mut x = seed.wrapping_mul(0x9E37_79B1) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^ (x >> 15)
}

fn unit(seed: u32) -> f32 {
    (hash(seed) % 10_000) as f32 / 10_000.0
}

fn hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i as i32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn smooth(edge: f32, x: f32) -> f32 {
    // 1 inside, 0 outside, with a one-pixel soft edge.
    (0.5 - x / edge).clamp(0.0, 1.0)
}

// A gradient disc with an abstract head-and-shoulders figure.
pub fn avatar(seed: u32) -> Image {
    let size = 160u32;
    let mut c = Canvas::new(size, size);
    let h1 = unit(seed);
    let a = hsv(h1, 0.55, 0.95);
    let b = hsv(h1 + 0.12 + unit(seed + 7) * 0.1, 0.7, 0.72);
    let s = size as f32;
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 / s, y as f32 / s);
            let mut rgb = mix(a, b, (fx * 0.4 + fy * 0.6).clamp(0.0, 1.0));
            let head = (((fx - 0.5).powi(2) + (fy - 0.37).powi(2)).sqrt() - 0.17) * s;
            let body = ((((fx - 0.5) / 0.36).powi(2) + ((fy - 1.02) / 0.38).powi(2)).sqrt() - 1.0) * 0.36 * s;
            let cover = smooth(1.5, head).max(smooth(1.5, body));
            rgb = mix(rgb, [1.0, 1.0, 1.0], cover * 0.88);
            c.put(x, y, rgb, 1.0);
        }
    }
    c.image()
}

// A landscape: sky, sun and layered ridges. Different seeds give different
// palettes and skylines.
pub fn photo(seed: u32, w: u32, h: u32) -> Image {
    let mut c = Canvas::new(w, h);
    let warm = seed.is_multiple_of(2);
    let (top, bottom) = if warm {
        (hsv(0.72 + unit(seed) * 0.05, 0.55, 0.45), hsv(0.05 + unit(seed + 1) * 0.04, 0.6, 1.0))
    } else {
        (hsv(0.58, 0.6, 0.6), hsv(0.52 + unit(seed + 1) * 0.05, 0.35, 0.98))
    };
    let (sw, sh) = (w as f32, h as f32);
    let sun = (sw * (0.25 + unit(seed + 3) * 0.5), sh * 0.42);
    let ridges: Vec<(f32, [f32; 3], f32, f32)> = (0..4)
        .map(|i| {
            let depth = i as f32 / 3.0;
            let base = sh * (0.50 + depth * 0.14);
            let tone = mix(hsv(0.62, 0.35, 0.55), hsv(0.40 + unit(seed + 9) * 0.1, 0.55, 0.22), depth);
            (base, tone, 0.012 + unit(seed + i * 11) * 0.01, unit(seed + 20 + i) * 6.0)
        })
        .collect();
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32, y as f32);
            let mut rgb = mix(top, bottom, (fy / (sh * 0.75)).clamp(0.0, 1.0));
            let d = ((fx - sun.0).powi(2) + (fy - sun.1).powi(2)).sqrt();
            rgb = mix(rgb, [1.0, 0.95, 0.8], (1.0 - d / (sh * 0.16)).clamp(0.0, 1.0).powi(2));
            for (i, (base, tone, freq, phase)) in ridges.iter().enumerate() {
                let ridge = base
                    + (fx * freq + phase).sin() * sh * 0.07
                    + (fx * freq * 2.3 + phase * 1.7).sin() * sh * 0.035
                    - (i as f32) * sh * 0.02;
                if fy > ridge {
                    let fog = ((fy - ridge) / (sh * 0.3)).clamp(0.0, 1.0);
                    rgb = mix(*tone, mix(*tone, bottom, 0.35), 1.0 - fog);
                }
            }
            c.put(x, y, rgb, 1.0);
        }
    }
    c.image()
}

// White bars on transparency: the .slint side colourises them, the same
// format the real voice-note waveform uses.
pub fn waveform(seed: u32) -> Image {
    const BARS: u32 = 44;
    const BAR_W: u32 = 3;
    const GAP: u32 = 2;
    const H: u32 = 30;
    let width = BARS * (BAR_W + GAP);
    let mut c = Canvas::new(width, H);
    for i in 0..BARS {
        let envelope = ((i as f32 / BARS as f32) * std::f32::consts::PI).sin().max(0.25);
        let level = (0.25 + unit(seed + i * 5) * 0.75) * envelope;
        let bar = ((level * H as f32).round() as u32).clamp(2, H);
        let top = (H - bar) / 2;
        for y in top..top + bar {
            for x in 0..BAR_W {
                c.put(i * (BAR_W + GAP) + x, y, [1.0, 1.0, 1.0], 1.0);
            }
        }
    }
    c.image()
}

// The vivid "wallpaper" glass is drawn over: saturated blue and red colour
// fields with a warm highlight, in the spirit of the Liquid Glass
// marketing art, so refraction and blur actually show. Light mode uses the
// same hues lifted toward pastel.
pub fn backdrop(w: u32, h: u32, dark: bool) -> Canvas {
    let mut c = Canvas::new(w, h);
    let (base_a, base_b) = if dark {
        ([0.02, 0.05, 0.40], [0.30, 0.03, 0.18])
    } else {
        ([0.45, 0.68, 1.00], [1.00, 0.62, 0.66])
    };
    let blobs: [(f32, f32, f32, [f32; 3], f32); 7] = if dark {
        [
            (0.10, 0.15, 0.40, [0.05, 0.35, 1.00], 1.0),
            (0.90, 0.10, 0.36, [1.00, 0.12, 0.25], 1.0),
            (0.80, 0.90, 0.46, [0.10, 0.45, 1.00], 1.0),
            (0.15, 0.88, 0.40, [0.95, 0.10, 0.30], 1.0),
            (0.50, 0.50, 0.28, [0.55, 0.20, 0.95], 0.85),
            (0.60, 0.15, 0.20, [1.00, 0.60, 0.30], 0.75),
            (0.35, 0.60, 0.18, [0.10, 0.85, 1.00], 0.65),
        ]
    } else {
        [
            (0.10, 0.15, 0.40, [0.20, 0.55, 1.00], 1.0),
            (0.90, 0.10, 0.36, [1.00, 0.35, 0.40], 1.0),
            (0.80, 0.90, 0.46, [0.35, 0.65, 1.00], 1.0),
            (0.15, 0.88, 0.40, [1.00, 0.40, 0.50], 1.0),
            (0.50, 0.50, 0.28, [0.80, 0.55, 1.00], 0.85),
            (0.60, 0.15, 0.20, [1.00, 0.80, 0.45], 0.75),
            (0.35, 0.60, 0.18, [0.45, 0.90, 1.00], 0.65),
        ]
    };
    let diag = ((w * w + h * h) as f32).sqrt();
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let mut rgb = mix(base_a, base_b, (fx * 0.5 + fy * 0.5).clamp(0.0, 1.0));
            for (bx, by, radius, color, strength) in blobs {
                let d = (((fx - bx) * w as f32).powi(2) + ((fy - by) * h as f32).powi(2)).sqrt();
                let k = 1.0 / (1.0 + (d / (radius * diag * 0.45)).powi(2));
                rgb = mix(rgb, color, (k * k * strength).clamp(0.0, 1.0));
            }
            c.put(x, y, rgb, 1.0);
        }
    }
    c
}
