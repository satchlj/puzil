//! The app-wide math engine. Formulas typeset on `puzil-math`'s worker thread; views
//! subscribe to [`MathHub`] and rebuild messages whose formulas just became ready.

use std::{collections::HashSet, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::*;
use puzil_math::{Engine, Key, MathSource};

pub struct MathHub {
    pub engine: Arc<Engine>,
    _pump: Task<()>,
}

/// Keys whose results are now available.
pub struct MathReady(pub Rc<HashSet<Key>>);

impl EventEmitter<MathReady> for MathHub {}

struct GlobalMath(Entity<MathHub>);
impl Global for GlobalMath {}

pub fn init(cx: &mut App) {
    let hub = cx.new(|cx| {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Key>();
        let engine = Arc::new(Engine::new(move |k| {
            let _ = tx.send(k);
        }));
        let pump = cx.spawn(async move |this, cx| {
            while let Some(k) = rx.recv().await {
                // Formulas finish in about 0.1 ms each: gather a frame's worth before telling
                // views, so a burst rebuilds each message once.
                cx.background_executor().timer(Duration::from_millis(16)).await;
                let mut keys: HashSet<Key> = [k].into();
                while let Ok(k) = rx.try_recv() {
                    keys.insert(k);
                }
                if this.update(cx, |_, cx| cx.emit(MathReady(Rc::new(keys)))).is_err() {
                    break;
                }
            }
        });
        MathHub { engine, _pump: pump }
    });
    cx.set_global(GlobalMath(hub));
}

pub fn hub(cx: &App) -> Entity<MathHub> {
    cx.global::<GlobalMath>().0.clone()
}

pub fn source(tex: &str, mathml: Option<&str>, display: bool) -> MathSource {
    MathSource { tex: tex.to_string(), mathml: mathml.map(str::to_string), display }
}
