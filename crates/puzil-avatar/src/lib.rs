//! Generated avatars for users who never uploaded one: a gradient whose own colour field is
//! bent into a motif (a swirl, ripples, marbling…). Every pixel is a blend of the palette;
//! nothing is drawn on top.

use image::{Rgba, RgbaImage};

/// Colours in OKLab, so blends stay even in lightness.
#[derive(Clone, Copy, Debug)]
struct Lab(f32, f32, f32);

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}
fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0., 1.);
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1. / 2.4) - 0.055 }
}

fn hex(h: u32) -> Lab {
    let (r, g, b) = (
        srgb_to_linear(((h >> 16) & 255) as f32 / 255.),
        srgb_to_linear(((h >> 8) & 255) as f32 / 255.),
        srgb_to_linear((h & 255) as f32 / 255.),
    );
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    Lab(
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    )
}

fn to_rgb(c: Lab) -> [u8; 3] {
    let l = c.0 + 0.3963377774 * c.1 + 0.2158037573 * c.2;
    let m = c.0 - 0.1055613458 * c.1 - 0.0638541728 * c.2;
    let s = c.0 - 0.0894841775 * c.1 - 1.2914855480 * c.2;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    let r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
    let g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
    let b = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s;
    [r, g, b].map(|v| (linear_to_srgb(v) * 255.).round() as u8)
}

fn mix(a: Lab, b: Lab, t: f32) -> Lab {
    Lab(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t)
}

/// Two hues the field swings between.
const DUOS: &[[u32; 2]] = &[
    [0x9e2b3f, 0x5e3fa3], // crimson / violet
    [0x6b7a2f, 0x4a5a9a], // olive / slate blue
];

/// Dark, mid, accent: one hue family from shadow to highlight.
const PALETTES: &[[u32; 3]] = &[
    [0x0f3b46, 0x1f6f78, 0x7fc8a9], // teal / jade
    [0x2a1b3d, 0x5b2a5e, 0xd8875f], // aubergine / ember
    [0x1d2b1f, 0x3e5c3a, 0xc9a227], // forest / gold
    [0x1b1f4b, 0x3d3aa8, 0xe0726b], // indigo / coral
    [0x3a1a12, 0x9a3f1f, 0xe3b27a], // rust / sand
    [0x0e1c2f, 0x2b4c7e, 0x8fb8de], // navy / ice
    [0x2e2a14, 0x6f6a2a, 0xd9c58b], // olive / ochre
    [0x1a0f24, 0x6b2d5c, 0xf0a07a], // plum / apricot
    [0x102a2a, 0x2e6b5e, 0xb5d66b], // pine / lime
    [0x1e1e2e, 0x46405e, 0xa99cc9], // slate / lavender
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motif {
    Swirl,
    Ripple,
    Marble,
    Wave,
    Eclipse,
    Vortex,
}

/// Puzil's own icon: the aubergine/ember wave in eight crisp bands.
pub fn app_icon_style() -> Style {
    Style::for_seed(4, 2).with_motif(Motif::Wave).with_palette(1).with_bands(8)
}

/// The motifs users get (eclipse read as a target).
pub const MOTIFS: [Motif; 5] = [Motif::Swirl, Motif::Ripple, Motif::Marble, Motif::Wave, Motif::Vortex];

/// Everything that varies between avatars, derived from a seed (the user id).
#[derive(Clone, Debug)]
pub struct Style {
    stops: [Lab; 3],
    pub motif: Motif,
    /// How much structure the field has: 1 (soft) to 3 (defined).
    pub level: u8,
    angle: f32,
    center: (f32, f32),
    phase: f32,
    spin: f32,
    noise_seed: u32,
    /// Hard-edged bands instead of a smooth gradient (the crisp app icon); 0 is smooth.
    bands: u8,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
}

impl Style {
    /// A user's avatar style. Busy motifs (marble) are drawn soft, the rest medium.
    pub fn for_user(user_id: u64) -> Self {
        let s = Self::for_seed(user_id, 2);
        let level = if s.motif == Motif::Marble { 1 } else { 2 };
        Self { level, ..s }
    }

    pub fn for_seed(seed: u64, level: u8) -> Self {
        let mut r = Rng(seed ^ 0x5157_4a11);
        let stops = palette((r.next() % (PALETTES.len() + DUOS.len()) as u64) as usize);
        Style {
            stops,
            motif: MOTIFS[(r.next() % MOTIFS.len() as u64) as usize],
            level: level.clamp(1, 3),
            angle: r.range(0., std::f32::consts::TAU),
            center: (r.range(-0.35, 0.35), r.range(-0.35, 0.35)),
            phase: r.range(0., std::f32::consts::TAU),
            spin: if r.f() < 0.5 { -1. } else { 1. },
            noise_seed: r.next() as u32,
            bands: 0,
        }
    }

    /// The number of palettes: one-hue ramps first, then two-hue pairs.
    pub fn palette_count() -> usize {
        PALETTES.len() + DUOS.len()
    }

    /// This style with palette `i` (see [`Style::palette_count`]).
    pub fn with_palette(mut self, i: usize) -> Self {
        self.stops = palette(i);
        self
    }

    /// The field cut into `n` flat bands with sharp (antialiased) edges.
    pub fn with_bands(mut self, n: u8) -> Self {
        self.bands = n;
        self
    }

    pub fn with_motif(mut self, m: Motif) -> Self {
        self.motif = m;
        self
    }

    /// The palette position at p (in [-1, 1]²), before mapping to colour.
    fn field(&self, x: f32, y: f32) -> f32 {
        let lv = self.level as f32;
        let (dx, dy) = (self.angle.cos(), self.angle.sin());
        let (cx, cy) = self.center;
        match self.motif {
            Motif::Swirl => {
                // Twist the plane around the centre, strongest near it, then read a linear
                // gradient: its bands wind into a spiral.
                let (px, py) = (x - cx, y - cy);
                let r2 = px * px + py * py;
                let twist = self.spin * (1.6 + 1.2 * lv) * (-r2 / 0.45).exp();
                let (s, c) = twist.sin_cos();
                let (qx, qy) = (px * c - py * s, px * s + py * c);
                (qx * dx + qy * dy) * (0.55 + 0.25 * lv)
            }
            Motif::Ripple => {
                let (px, py) = (x - cx, y - cy);
                let r = (px * px + py * py).sqrt();
                let a = py.atan2(px);
                (r + 0.08 * (2. * a + self.phase).sin()) * (0.7 + 0.55 * lv)
            }
            Motif::Marble => {
                let s = 1.3 + 0.4 * lv;
                let w1 = fbm(x * s, y * s, self.noise_seed, 4);
                let w2 = fbm(x * s + 5.2, y * s + 1.3, self.noise_seed, 4);
                let (qx, qy) = (x + (0.45 + 0.25 * lv) * w1, y + (0.45 + 0.25 * lv) * w2);
                (qx * dx + qy * dy) * (0.5 + 0.2 * lv)
            }
            Motif::Wave => {
                let u = x * dx + y * dy;
                let v = -x * dy + y * dx;
                (u + (0.12 + 0.05 * lv) * (v * (2.5 + lv) + self.phase).sin()) * (0.5 + 0.35 * lv)
            }
            Motif::Eclipse => {
                let (px, py) = (x - cx * 0.6, y - cy * 0.6);
                let r = (px * px + py * py).sqrt();
                // A soft disc with a bright rim: one band up, one down.
                let rim = 0.5 + 0.06 * lv;
                let k = 0.2 / lv;
                let t = 1. / (1. + ((r - rim) / k).exp());
                0.15 + (1. - t) * 0.5 + (-((r - rim) / (0.05 + k * 0.4)).powi(2)).exp() * 0.35
            }
            Motif::Vortex => {
                // Two opposite twists, like a pair of eddies.
                let mut qx = x;
                let mut qy = y;
                for (ox, oy, sgn) in [(cx - 0.3, cy, 1.), (cx + 0.3, -cy, -1.)] {
                    let (px, py) = (qx - ox, qy - oy);
                    let r2 = px * px + py * py;
                    let tw = sgn * self.spin * (1.2 + 0.9 * lv) * (-r2 / 0.18).exp();
                    let (s, c) = tw.sin_cos();
                    qx = ox + px * c - py * s;
                    qy = oy + px * s + py * c;
                }
                (qx * dx + qy * dy) * (0.55 + 0.25 * lv)
            }
        }
    }

    /// Palette position to colour. The position wraps smoothly dark → mid → accent → mid →
    /// dark, so extra bands never show a seam.
    fn color(&self, t: f32) -> Lab {
        let mut w = (t * 0.5 + 0.5).rem_euclid(1.0);
        if self.bands > 0 {
            let n = self.bands as f32;
            w = (w * n).round() / n;
        }
        let u = w * 4.;
        let seg = u.floor() as i32;
        let f = smooth(u - seg as f32);
        let [a, b, c] = self.stops;
        match seg {
            0 => mix(a, b, f),
            1 => mix(b, c, f),
            2 => mix(c, b, f),
            _ => mix(b, a, f),
        }
    }

    /// A `size`×`size` round avatar.
    pub fn render(&self, size: u32) -> RgbaImage {
        self.render_shaped(size, true)
    }

    /// An app icon on Apple's grid: the field in a continuous-corner squircle (a superellipse)
    /// inset 100/1024 from each edge, over a soft shadow, as macOS icons are drawn.
    pub fn render_icon(&self, size: u32) -> RgbaImage {
        const SS: u32 = 3;
        let mut img = RgbaImage::new(size, size);
        let s = size as f32;
        let (inset, body) = (s * 100. / 1024., s * 824. / 1024.);
        let shape = |px: f32, py: f32| -> f32 {
            // Superellipse value: < 1 inside.
            let x = ((px - inset) / body * 2. - 1.).abs();
            let y = ((py - inset) / body * 2. - 1.).abs();
            x.powf(5.) + y.powf(5.)
        };
        for py in 0..size {
            for px in 0..size {
                let (mut acc, mut cov) = ([0f32; 3], 0f32);
                for sy in 0..SS {
                    for sx in 0..SS {
                        let fx = px as f32 + (sx as f32 + 0.5) / SS as f32;
                        let fy = py as f32 + (sy as f32 + 0.5) / SS as f32;
                        if shape(fx, fy) > 1. {
                            continue;
                        }
                        let x = (fx - inset) / body * 2. - 1.;
                        let y = (fy - inset) / body * 2. - 1.;
                        let mut c = self.color(self.field(x * 0.9, y * 0.9));
                        // Light from above, a touch darker toward the bottom edge.
                        c.0 += 0.06 * (0.5 - y * 0.5) - 0.03;
                        let rgb = to_rgb(c);
                        for i in 0..3 {
                            acc[i] += rgb[i] as f32;
                        }
                        cov += 1.;
                    }
                }
                let a = cov / (SS * SS) as f32;
                if a > 0. {
                    img.put_pixel(px, py, Rgba([(acc[0] / cov) as u8, (acc[1] / cov) as u8, (acc[2] / cov) as u8, (a * 255.) as u8]));
                } else {
                    // The shadow: the shape moved down a little, its edge softened.
                    let v = shape(px as f32 + 0.5, py as f32 + 0.5 - s * 10. / 1024.);
                    let shadow = (1. - ((v - 1.) / 0.35).clamp(0., 1.)) * 0.28;
                    if shadow > 0. {
                        img.put_pixel(px, py, Rgba([0, 0, 0, (shadow * 255.) as u8]));
                    }
                }
            }
        }
        img
    }

    /// Round, or square (for tiles that clip to their own shape, like the server icon).
    pub fn render_shaped(&self, size: u32, round: bool) -> RgbaImage {
        const SS: u32 = 3; // supersamples per axis
        let mut img = RgbaImage::new(size, size);
        let n = (size * SS) as f32;
        for py in 0..size {
            for px in 0..size {
                let (mut acc, mut cov) = ([0f32; 3], 0f32);
                for sy in 0..SS {
                    for sx in 0..SS {
                        let x = ((px * SS + sx) as f32 + 0.5) / n * 2. - 1.;
                        let y = ((py * SS + sy) as f32 + 0.5) / n * 2. - 1.;
                        if round && x * x + y * y > 1. {
                            continue;
                        }
                        let mut c = self.color(self.field(x, y));
                        // A faint light from the upper left, for depth.
                        c.0 += 0.05 * (1. - ((x + 0.5).powi(2) + (y + 0.6).powi(2)).sqrt()).max(0.);
                        let rgb = to_rgb(c);
                        for i in 0..3 {
                            acc[i] += rgb[i] as f32;
                        }
                        cov += 1.;
                    }
                }
                if cov > 0. {
                    let a = cov / (SS * SS) as f32;
                    img.put_pixel(px, py, Rgba([(acc[0] / cov) as u8, (acc[1] / cov) as u8, (acc[2] / cov) as u8, (a * 255.) as u8]));
                }
            }
        }
        img
    }
}

fn palette(i: usize) -> [Lab; 3] {
    match PALETTES.get(i) {
        Some(p) => p.map(hex),
        None => {
            let [a, b] = DUOS[(i - PALETTES.len()) % DUOS.len()].map(hex);
            // The midpoint dips a little, so the swing between the hues has depth.
            let mut m = mix(a, b, 0.5);
            m.0 -= 0.06;
            [a, m, b]
        }
    }
}

fn smooth(t: f32) -> f32 {
    t * t * (3. - 2. * t)
}

fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374761393) ^ (y as u32).wrapping_mul(668265263) ^ seed.wrapping_mul(2246822519);
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    h ^= h >> 16;
    (h & 0xffff) as f32 / 65535. * 2. - 1.
}

fn noise(x: f32, y: f32, seed: u32) -> f32 {
    let (xi, yi) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (smooth(x - xi as f32), smooth(y - yi as f32));
    let a = hash(xi, yi, seed);
    let b = hash(xi + 1, yi, seed);
    let c = hash(xi, yi + 1, seed);
    let d = hash(xi + 1, yi + 1, seed);
    a + (b - a) * fx + (c - a) * fy + (a - b - c + d) * fx * fy
}

fn fbm(x: f32, y: f32, seed: u32, oct: u32) -> f32 {
    let (mut s, mut amp, mut f) = (0., 0.5, 1.);
    for i in 0..oct {
        s += amp * noise(x * f, y * f, seed.wrapping_add(i * 97));
        amp *= 0.5;
        f *= 2.;
    }
    s
}

/// Whether `img` looks like a generated identicon (Zulip's and Gravatar's defaults): a few
/// flat colours laid out with fourfold rotational symmetry. Photos and drawn logos fail one
/// test or the other.
pub fn looks_generated(img: &RgbaImage) -> bool {
    let (w, h) = img.dimensions();
    if w != h || w < 16 || w > 512 {
        return false;
    }
    let mut counts: std::collections::HashMap<[u8; 3], usize> = std::collections::HashMap::new();
    for p in img.pixels() {
        if p[3] < 128 {
            continue;
        }
        *counts.entry([p[0] & 0xf0, p[1] & 0xf0, p[2] & 0xf0]).or_default() += 1;
    }
    let total: usize = counts.values().sum();
    let mut v: Vec<usize> = counts.into_values().collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    let top: usize = v.iter().take(4).sum();
    let flat = total > 0 && top as f32 / total as f32 > 0.99;
    // Compare with itself turned a quarter.
    let mut diff = 0u64;
    for y in 0..h {
        for x in 0..w {
            let a = img.get_pixel(x, y);
            let b = img.get_pixel(w - 1 - y, x);
            diff += (0..3).map(|i| (a[i] as i32 - b[i] as i32).unsigned_abs() as u64).sum::<u64>();
        }
    }
    let mean = diff as f32 / (w * h * 3) as f32;
    flat && mean < 3.
}

/// Share of opaque pixels in the four most common (coarsened) colours: near 1 for flat
/// generated art, far lower for photos.
pub fn flatness(img: &RgbaImage) -> f32 {
    let mut counts: std::collections::HashMap<[u8; 3], usize> = std::collections::HashMap::new();
    for p in img.pixels().filter(|p| p[3] >= 128) {
        *counts.entry([p[0] & 0xf0, p[1] & 0xf0, p[2] & 0xf0]).or_default() += 1;
    }
    let total: usize = counts.values().sum();
    let mut v: Vec<usize> = counts.into_values().collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v.iter().take(4).sum::<usize>() as f32 / total.max(1) as f32
}

#[doc(hidden)]
pub fn generated_stats(img: &RgbaImage) -> (f32, f32) {
    let (w, h) = img.dimensions();
    let mut counts: std::collections::HashMap<[u8; 3], usize> = std::collections::HashMap::new();
    for p in img.pixels() {
        if p[3] >= 128 {
            *counts.entry([p[0] & 0xf0, p[1] & 0xf0, p[2] & 0xf0]).or_default() += 1;
        }
    }
    let total: usize = counts.values().sum();
    let mut v: Vec<usize> = counts.into_values().collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    let top: usize = v.iter().take(4).sum();
    let mut diff = 0u64;
    for y in 0..h {
        for x in 0..w {
            let a = img.get_pixel(x, y);
            let b = img.get_pixel(w - 1 - y, x);
            diff += (0..3).map(|i| (a[i] as i32 - b[i] as i32).unsigned_abs() as u64).sum::<u64>();
        }
    }
    (top as f32 / total.max(1) as f32, diff as f32 / (w * h * 3) as f32)
}
