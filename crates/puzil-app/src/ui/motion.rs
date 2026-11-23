//! Shared motion: short entrances, nothing on the path to content. GPUI's animations already
//! honour the system's Reduce Motion setting (gpui-kit reads it at startup).

use std::time::Duration;

use gpui_kit::*;

/// A popover's entrance: fades in while settling 4px from where it was opened.
pub fn pop_in<E: IntoElement + Styled + 'static>(id: impl Into<ElementId>, el: E) -> AnimationElement<E> {
    el.with_animation(id, Animation::new(Duration::from_millis(130)).with_easing(ease_out_quint()), |el, d| {
        el.opacity(d).relative().top(px(4. * (1. - d)))
    })
}

/// A fade alone, for elements positioned absolutely (an offset would move them).
pub fn fade_in<E: IntoElement + Styled + 'static>(id: impl Into<ElementId>, el: E, ms: u64) -> AnimationElement<E> {
    el.with_animation(id, Animation::new(Duration::from_millis(ms)).with_easing(ease_out_quint()), |el, d| el.opacity(d))
}

/// A highlight that fades away (the jump-to-message flash).
pub fn fade_out<E: IntoElement + Styled + 'static>(id: impl Into<ElementId>, el: E, ms: u64) -> AnimationElement<E> {
    el.with_animation(id, Animation::new(Duration::from_millis(ms)).with_easing(ease_in_out), |el, d| el.opacity(1. - d))
}

/// `arrive`, starting `delay_ms` late: rows revealed together settle one after another.
pub fn arrive_after<E: IntoElement + Styled + 'static>(id: impl Into<ElementId>, el: E, rise: f32, delay_ms: u64) -> AnimationElement<E> {
    let total = 160 + delay_ms;
    let lead = delay_ms as f32 / total as f32;
    el.with_animation(id, Animation::new(Duration::from_millis(total)), move |el, t| {
        let d = ((t - lead) / (1. - lead)).clamp(0., 1.);
        let d = 1. - (1. - d).powi(5);
        el.opacity(d).relative().top(px(rise * (1. - d)))
    })
}

/// Content that just arrived (an image, a revealed row): a short fade with a small rise.
pub fn arrive<E: IntoElement + Styled + 'static>(id: impl Into<ElementId>, el: E, rise: f32) -> AnimationElement<E> {
    el.with_animation(id, Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()), move |el, d| {
        el.opacity(d).relative().top(px(rise * (1. - d)))
    })
}
