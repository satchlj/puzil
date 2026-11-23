//! The app icon: aubergine/ember palette, soft wave. `icons OUT_DIR` writes icon-{size}.png.
use puzil_avatar::{Motif, Style};

fn main() {
    let dir = std::env::args().nth(1).expect("output dir");
    let st = Style::for_seed(4, 1).with_motif(Motif::Wave).with_palette(1);
    for size in [1024u32, 512, 256, 128, 64, 32, 16] {
        st.render_icon(size).save(format!("{dir}/icon-{size}.png")).unwrap();
    }
}
