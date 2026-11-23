//! The app icon: aubergine/ember palette, a wave cut into eight crisp bands. `icons OUT_DIR` writes icon-{size}.png.

fn main() {
    let dir = std::env::args().nth(1).expect("output dir");
    let st = puzil_avatar::app_icon_style();
    for size in [1024u32, 512, 256, 128, 64, 32, 16] {
        st.render_icon(size).save(format!("{dir}/icon-{size}.png")).unwrap();
    }
}
