//! Round avatars: the picture once it's ready, a quiet tinted disc until then. Pictures that
//! just arrived fade in; ones already seen (scrolled back into view) just appear.

use std::sync::Arc;

use gpui_kit::*;

pub fn avatar(img: Option<Arc<RenderImage>>, size: f32, color: Hsla, _name: &str) -> Div {
    let d = div().size(px(size)).flex_none().rounded_full().overflow_hidden();
    match img {
        Some(i) => {
            let fresh = crate::media::just_arrived(&i);
            let id = ElementId::Integer(Arc::as_ptr(&i) as u64);
            let pic = gpui_kit::img(ImageSource::Render(i)).object_fit(ObjectFit::Cover).size_full().rounded_full();
            if fresh { d.bg(color.opacity(0.25)).child(super::motion::arrive(id, pic, 0.)) } else { d.child(pic) }
        }
        None => d.bg(color.opacity(0.25)),
    }
}

/// A presence-style dot at the avatar's lower right, ringed in `ring` (the surface colour).
pub fn with_status(av: Div, size: f32, dot: Hsla, ring: Hsla) -> Div {
    let d = (size * 0.3).max(10.);
    div().relative().flex_none().child(av).child(
        div()
            .absolute()
            .right(px(-2.))
            .bottom(px(-2.))
            .size(px(d + 4.))
            .rounded_full()
            .bg(ring)
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(d)).rounded_full().bg(dot)),
    )
}
