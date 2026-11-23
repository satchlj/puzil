//! Writes a contact sheet: every motif × level at 64px and 32px, several seeds each.
//! `cargo run -p puzil-avatar --example sheet --release -- OUT.png`

use image::{Rgba, RgbaImage, imageops};
use puzil_avatar::{MOTIFS, Style};

fn main() {
    let out = std::env::args().nth(1).expect("output path");
    let seeds: Vec<u64> = (0..8).map(|i| 1000 + i * 7919).collect();
    let (big, small, gap) = (64u32, 32u32, 10u32);
    let row_h = big + small + gap * 2;
    let rows = MOTIFS.len() as u32 * 3;
    let w = gap + seeds.len() as u32 * (big + gap);
    let mut sheet = RgbaImage::from_pixel(w, rows * row_h + gap, Rgba([24, 25, 28, 255]));
    let mut y = gap;
    for m in MOTIFS {
        for level in 1..=3u8 {
            for (i, s) in seeds.iter().enumerate() {
                let st = Style::for_seed(*s, level).with_motif(m);
                let x = gap + i as u32 * (big + gap);
                imageops::overlay(&mut sheet, &st.render(big), x as i64, y as i64);
                imageops::overlay(&mut sheet, &st.render(small), (x + (big - small) / 2) as i64, (y + big + gap) as i64);
            }
            y += row_h;
        }
    }
    sheet.save(&out).expect("save");
}
