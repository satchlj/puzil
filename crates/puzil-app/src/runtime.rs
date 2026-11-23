//! The tokio runtime that reqwest needs, bridged to GPUI's executor.

use std::{future::Future, sync::OnceLock};

pub fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("puzil-net")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

/// Runs `f` on tokio and awaits it from any executor, GPUI's included.
pub fn on_tokio<T: Send + 'static>(f: impl Future<Output = T> + Send + 'static) -> impl Future<Output = T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    rt().spawn(async move {
        let _ = tx.send(f.await);
    });
    async move { rx.await.expect("tokio task dropped") }
}
