//! Prints each image's flatness, quarter-turn difference and verdict.
fn main() {
    for p in std::env::args().skip(1) {
        let img = image::open(&p).expect("open").into_rgba8();
        let (flat, diff) = puzil_avatar::generated_stats(&img);
        println!("{:>6.3} {:>6.1} {:5} {}", flat, diff, puzil_avatar::looks_generated(&img), std::path::Path::new(&p).file_name().unwrap().to_string_lossy());
    }
}
