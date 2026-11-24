//! Lucide icons: the few Puzil draws, on top of gpui-kit's component set.

use std::borrow::Cow;

use gpui_kit::{prelude::FluentBuilder as _, *};

gpui_kit::assets::icon_assets!(
    Extra,
    [ArrowBigDown, ArrowBigUp, AtSign, CirclePlus, CornerUpLeft, Hash, MessagesSquare, Pencil, Trash, Users, Bot, Lock, Link, Mail, FaceSlightlySmilingPlus, Compass, CheckCheck, BellOff, Bell, CircleCheck, LogOut, VolumeX, Search, Palette, Monitor, Sun, Moon, Building2, User, Settings2, PanelLeft, Minus, Plus, ArrowDown, ArrowRight, FolderInput, Volume2, X,]
);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(b) = Extra.load(path)? {
            return Ok(Some(b));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut v = gpui_kit::assets::Assets.list(path)?;
        v.extend(Extra.list(path)?);
        v.sort();
        v.dedup();
        Ok(v)
    }
}

/// Discord's channel glyph: a `#`, with a small lock at its upper right for a private
/// channel. `bg` is the surface behind it (the lock is cut out of the hash with it).
pub fn channel(private: bool, size: f32, color: Hsla, bg: Hsla) -> Div {
    let lock = size * 0.55;
    div().relative().size(px(size)).flex_none().child(icon("hash", size, color)).when(private, |d| {
        d.child(
            div()
                .absolute()
                .top(px(-size * 0.12))
                .right(px(-size * 0.14))
                .size(px(lock + 2.))
                .rounded_full()
                .bg(bg)
                .flex()
                .items_center()
                .justify_center()
                .child(icon("lock", lock, color)),
        )
    })
}

/// The app icon drawn `points` wide, rendered at that size on a 2× display (cached per size):
/// its band edges stay sharp, which a big image scaled down on the GPU would not.
pub fn app_icon(points: f32) -> std::sync::Arc<RenderImage> {
    static CACHE: std::sync::Mutex<Vec<(u32, std::sync::Arc<RenderImage>)>> = std::sync::Mutex::new(Vec::new());
    let px = (points * 2.).round() as u32;
    let mut cache = CACHE.lock().unwrap();
    if let Some((_, i)) = cache.iter().find(|(s, _)| *s == px) {
        return i.clone();
    }
    let mut rgba = puzil_avatar::app_icon_style().render_icon(px);
    for p in rgba.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    let img = std::sync::Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(rgba)]));
    cache.push((px, img.clone()));
    img
}

/// An icon by its Lucide file name (`"corner-up-left"`), tinted with the text colour.
pub fn icon(name: &'static str, size: f32, color: Hsla) -> Svg {
    svg().path(format!("icons/{name}.svg")).size(px(size)).flex_none().text_color(color)
}
