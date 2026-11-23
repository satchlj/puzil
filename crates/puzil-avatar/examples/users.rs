//! Contact sheet of avatars as users would get them (`Style::for_user`), 64px over 32px.
use image::{Rgba, RgbaImage, imageops};
use puzil_avatar::Style;

fn main() {
    let out = std::env::args().nth(1).expect("output path");
    let (cols, rows, big, small, gap) = (12u32, 4u32, 64u32, 32u32, 10u32);
    let row_h = big + small + gap * 2;
    let mut sheet = RgbaImage::from_pixel(gap + cols * (big + gap), gap + rows * row_h, Rgba([24, 25, 28, 255]));
    for i in 0..cols * rows {
        let st = Style::for_user(1_200_000 + i as u64 * 7_331);
        let (x, y) = (gap + (i % cols) * (big + gap), gap + (i / cols) * row_h);
        imageops::overlay(&mut sheet, &st.render(big), x as i64, y as i64);
        imageops::overlay(&mut sheet, &st.render(small), (x + (big - small) / 2) as i64, (y + big + gap) as i64);
    }
    sheet.save(&out).expect("save");
}
