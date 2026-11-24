//! Benchmark mode: `PUZIL_BENCH=out.json puzil-app` runs a fixed script against the local
//! cache, writes its measurements as JSON and quits.
//!
//! It can't send anything. [`prepare`] turns the network off before any client exists (the
//! Zulip client refuses every request; the other HTTP clients get a dead proxy), the session
//! starts no queue, overlay or fetch, reads aren't marked, and the script never types or
//! clicks. Point `PUZIL_DATA_DIR` at a copy of the data: the run still writes drafts, window
//! frames and sidebar state there.
//!
//! The script: wait for launch to settle, note memory, then open the conversations with the
//! most cached messages one at a time, timing how long each takes to paint, and scroll each
//! from the bottom up and back, one step per frame, timing every frame. Scrolling goes through
//! the same path as a trackpad: scroll events dispatched to the window over the message list.
//! (`ListState::scroll_by` from the bottom of a bottom-aligned list doesn't move it: each step
//! starts again from the end.)

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

use gpui_kit::*;
use serde_json::{Value, json};

use crate::{Root, ui::conversation::Target};

static RESULTS: Mutex<BTreeMap<String, f64>> = Mutex::new(BTreeMap::new());
static SAMPLES: Mutex<BTreeMap<&'static str, Vec<f64>>> = Mutex::new(BTreeMap::new());

/// When the frame being built began (the root view's render), and which phase of the script
/// is running, so frame work is filed under it.
static FRAME_START: Mutex<Option<Instant>> = Mutex::new(None);
static PHASE: Mutex<Option<&'static str>> = Mutex::new(None);

fn set_phase(p: Option<&'static str>) {
    *PHASE.lock().unwrap() = p;
}

/// Called at the start of the root view's render.
pub fn frame_begins() {
    if active() {
        *FRAME_START.lock().unwrap() = Some(Instant::now());
    }
}

/// The last element painted in the window: closes the frame's work time (render, layout,
/// prepaint and paint of the whole tree, without the GPU's part).
pub fn frame_end_marker() -> impl IntoElement {
    canvas(
        |_, _, _| {},
        |_, _, _, _| {
            let Some(start) = FRAME_START.lock().unwrap().take() else { return };
            let ms = start.elapsed().as_secs_f64() * 1000.;
            let name = match *PHASE.lock().unwrap() {
                Some(p) => p,
                None => "frame_work_ms_other",
            };
            SAMPLES.lock().unwrap().entry(name).or_default().push(ms);
        },
    )
    .absolute()
    .size_0()
}

/// Conversations opened, steps per scroll direction, and pixels per step.
const CONVERSATIONS: usize = 5;
const SCROLL_STEPS: usize = 150;
const SCROLL_STEP_PX: f32 = 40.;
/// Where scroll events land: over the message list (the window is 1280×860; the sidebar is on
/// the left, the composer at the bottom).
const SCROLL_AT: (f32, f32) = (780., 400.);

fn output() -> Option<PathBuf> {
    std::env::var_os("PUZIL_BENCH").map(PathBuf::from)
}

pub fn active() -> bool {
    output().is_some()
}

/// First thing in `main`, before any thread or HTTP client exists.
pub fn prepare() {
    if !active() {
        return;
    }
    puzil_zulip::client::set_offline();
    // SAFETY: no other thread exists yet.
    unsafe {
        for k in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
            std::env::set_var(k, "http://127.0.0.1:9");
        }
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
        std::env::set_var("PUZIL_NO_AUTO_READ", "1");
        std::env::set_var("PUZIL_NO_UPDATE_CHECK", "1");
    }
}



/// Milliseconds since the process started (before `main`: dyld loading counts).
pub fn since_exec_ms() -> f64 {
    #[cfg(target_os = "macos")]
    {
        let mut info = RusageInfoV0::default();
        #[repr(C)]
        struct Timebase {
            numer: u32,
            denom: u32,
        }
        unsafe extern "C" {
            fn mach_absolute_time() -> u64;
            fn mach_timebase_info(info: *mut Timebase) -> i32;
        }
        let mut tb = Timebase { numer: 0, denom: 0 };
        // SAFETY: flavor 0 fills exactly `RusageInfoV0`; the timebase call fills its struct.
        let ok = unsafe { proc_pid_rusage(std::process::id() as i32, 0, &mut info) == 0 && mach_timebase_info(&mut tb) == 0 };
        if ok && tb.denom > 0 {
            let now = unsafe { mach_absolute_time() };
            return now.saturating_sub(info.proc_start_abstime) as f64 * tb.numer as f64 / tb.denom as f64 / 1e6;
        }
    }
    0.
}

/// Records how long after the process started the launch reached `step`
/// (as `launch_<step>_ms`).
pub fn mark(step: &str) {
    if active() {
        record(&format!("launch_{step}_ms"), since_exec_ms());
    }
}

/// Notes a one-off measurement.
pub fn record(name: &str, value: f64) {
    if active() {
        RESULTS.lock().unwrap().entry(name.to_string()).or_insert(value);
    }
}

/// Times a scope into the named sample set (microseconds), while a benchmark runs.
pub struct Timer(Option<(&'static str, Instant)>);

impl Timer {
    pub fn start(name: &'static str) -> Self {
        Self(active().then(|| (name, Instant::now())))
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some((name, t)) = self.0 {
            SAMPLES.lock().unwrap().entry(name).or_default().push(t.elapsed().as_secs_f64() * 1e6);
        }
    }
}

#[cfg(target_os = "macos")]
// `rusage_info_v0` from <libproc.h>.
#[repr(C)]
#[derive(Default)]
struct RusageInfoV0 {
    uuid: [u8; 16],
    user_time: u64,
    system_time: u64,
    pkg_idle_wkups: u64,
    interrupt_wkups: u64,
    pageins: u64,
    wired_size: u64,
    resident_size: u64,
    phys_footprint: u64,
    proc_start_abstime: u64,
    proc_exit_abstime: u64,
}
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut RusageInfoV0) -> i32;
}

/// Physical footprint in MB: what Activity Monitor shows as Memory and what macOS weighs
/// under memory pressure. Unlike RSS it counts GPU memory (window surfaces, textures) and
/// leaves out shared system libraries.
#[cfg(target_os = "macos")]
fn footprint_mb() -> f64 {
    let mut info = RusageInfoV0::default();
    // SAFETY: flavor 0 (RUSAGE_INFO_V0) fills exactly this struct.
    if unsafe { proc_pid_rusage(std::process::id() as i32, 0, &mut info) } == 0 {
        info.phys_footprint as f64 / (1024. * 1024.)
    } else {
        0.
    }
}

#[cfg(not(target_os = "macos"))]
fn footprint_mb() -> f64 {
    rss_mb()
}

/// CPU time the process has used, user and system, in seconds.
fn cpu_seconds() -> f64 {
    #[repr(C)]
    struct Timeval {
        sec: i64,
        usec: i32,
    }
    #[repr(C)]
    struct Rusage {
        utime: Timeval,
        stime: Timeval,
        rest: [i64; 14],
    }
    unsafe extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }
    let mut u = std::mem::MaybeUninit::<Rusage>::zeroed();
    // SAFETY: RUSAGE_SELF (0) fills a struct rusage, which this matches in size and layout.
    if unsafe { getrusage(0, u.as_mut_ptr()) } != 0 {
        return 0.;
    }
    let u = unsafe { u.assume_init() };
    let t = |t: &Timeval| t.sec as f64 + t.usec as f64 / 1e6;
    t(&u.utime) + t(&u.stime)
}

fn rss_mb() -> f64 {
    let out = std::process::Command::new("/bin/ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output();
    out.ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok()).map(|kb| kb / 1024.).unwrap_or(0.)
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i]
}

fn summary(mut v: Vec<f64>) -> Value {
    v.sort_by(f64::total_cmp);
    json!({ "n": v.len(), "p50": pct(&v, 0.5), "p95": pct(&v, 0.95), "p99": pct(&v, 0.99), "max": v.last().copied().unwrap_or(0.) })
}

/// Runs `f` in the window, then resolves when the next frame has been drawn.
async fn frame(root: &WeakEntity<Root>, cx: &mut AsyncWindowContext, f: impl FnOnce(&mut Root, &mut Window, &mut Context<Root>)) -> Option<Instant> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    root.update_in(cx, |r, window, cx| {
        f(r, window, cx);
        window.on_next_frame(move |_, _| {
            let _ = tx.send(Instant::now());
        });
        // A frame without `window.refresh()`, which would redraw every view and so hide what
        // view caching saves.
        cx.notify();
    })
    .ok()?;
    rx.await.ok()
}

async fn sleep(cx: &mut AsyncWindowContext, ms: u64) {
    cx.background_executor().timer(Duration::from_millis(ms)).await;
}

pub fn start(root: WeakEntity<Root>, window: &mut Window, cx: &mut Context<Root>) {
    cx.spawn_in(window, async move |_, cx| {
        // PUZIL_BENCH_LAUNCH_ONLY: just the launch (for profiling it), then quit.
        let report = if std::env::var_os("PUZIL_BENCH_LAUNCH_ONLY").is_some() {
            sleep(cx, 300).await;
            json!([])
        } else {
            run(&root, cx).await
        };
        let mut out = json!({
            "version": env!("CARGO_PKG_VERSION"),
            "conversations": report,
        });
        for (k, v) in RESULTS.lock().unwrap().iter() {
            out[k] = json!(v);
        }
        for (k, v) in SAMPLES.lock().unwrap().iter() {
            out[*k] = summary(v.clone());
        }
        if let Some(path) = output() {
            match std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap_or_default()) {
                Ok(()) => tracing::info!("benchmark written to {}", path.display()),
                Err(e) => tracing::error!("writing the benchmark: {e}"),
            }
        }
        // PUZIL_BENCH_HOLD=secs keeps the app open at the end, to inspect its memory.
        if let Some(secs) = std::env::var("PUZIL_BENCH_HOLD").ok().and_then(|s| s.parse::<u64>().ok()) {
            tracing::info!("benchmark holding for {secs}s (pid {})", std::process::id());
            sleep(cx, secs * 1000).await;
        }
        let _ = cx.update(|_, cx| cx.quit());
    })
    .detach();
}

async fn run(root: &WeakEntity<Root>, cx: &mut AsyncWindowContext) -> Value {
    // Launch settles: the sidebar, avatars from the disk cache, the first layout passes.
    sleep(cx, 1500).await;
    record("idle_footprint_mb", footprint_mb());
    record("idle_rss_mb", rss_mb());
    let Ok(Some((workspace, store))) = root.update(cx, |r, cx| {
        let s = r.servers.get(r.active)?;
        Some((s.workspace.clone(), s.session.read(cx).store.clone()))
    }) else {
        tracing::error!("benchmark: no server to run against");
        return json!([]);
    };
    // PUZIL_BENCH_OPENS=n: open the n busiest conversations without scrolling (for profiling
    // opening a conversation).
    let opens_only: Option<usize> = std::env::var("PUZIL_BENCH_OPENS").ok().and_then(|s| s.parse().ok());
    let convs = store.busiest_convs(opens_only.unwrap_or(CONVERSATIONS)).unwrap_or_default();
    let mut report = Vec::new();
    let (mut opens, mut intervals) = (Vec::new(), Vec::new());
    for (conv, cached) in convs {
        // Open, then wait for the first frame that shows its messages.
        let t0 = Instant::now();
        set_phase(Some("open_frame_work_ms"));
        let ws = workspace.clone();
        let target = Target::Conv(conv.clone());
        let mut at = frame(root, cx, move |_, window, cx| ws.update(cx, |w, cx| w.open(target, window, cx))).await;
        let mut open_ms = None;
        // The first drawn frame whose conversation holds messages.
        while t0.elapsed() < Duration::from_secs(5) {
            let shown = workspace.read_with(cx, |w, cx| w.current_conversation().map(|c| c.read(cx).bench_message_count()).unwrap_or(0));
            if shown > 0 {
                open_ms = at.map(|a| (a - t0).as_secs_f64() * 1000.);
                break;
            }
            at = frame(root, cx, |_, _, _| {}).await;
        }
        let Some(conversation) = workspace.read_with(cx, |w, _| w.current_conversation()) else { continue };
        let shown = conversation.read_with(cx, |c, _| c.bench_message_count());
        // Let images and math from the cache arrive, then scroll up and back down.
        set_phase(None);
        if opens_only.is_some() {
            if let Some(ms) = open_ms {
                opens.push(ms);
            }
            report.push(json!({ "conversation": format!("{conv:?}"), "cached_messages": cached, "messages_shown": shown, "open_ms": open_ms }));
            sleep(cx, 200).await;
            continue;
        }
        sleep(cx, 500).await;
        let c = conversation.clone();
        frame(root, cx, move |_, _, cx| c.read(cx).bench_scroll_to_end()).await;
        let mut stamps = Vec::with_capacity(SCROLL_STEPS * 2 + 1);
        set_phase(Some("scroll_frame_work_ms"));
        // Positive deltas scroll up (towards older messages), as a trackpad does.
        for dy in [SCROLL_STEP_PX, -SCROLL_STEP_PX] {
            for _ in 0..SCROLL_STEPS {
                let c = conversation.clone();
                if let Some(t) = frame(root, cx, move |_, window, cx| {
                    let event = ScrollEvent {
                        position: point(px(SCROLL_AT.0), px(SCROLL_AT.1)),
                        delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
                        ..Default::default()
                    };
                    window.dispatch_event(PlatformInput::Scroll(event), cx);
                    if std::env::var_os("PUZIL_BENCH_TRACE").is_some() {
                        tracing::info!("bench scroll {dy} -> {:?}", c.read(cx).bench_scroll_top());
                    }
                })
                .await
                {
                    stamps.push(t);
                }
            }
        }
        set_phase(None);
        // A fixed amount of CPU work on the main thread, right after the scroll pass: the
        // same code takes longer on an efficiency core or at a low clock, so this says how
        // fast the main thread ran.
        let _ = root.update(cx, |_, _| {
            let t = Instant::now();
            let mut x = 0x9e37_79b9_7f4a_7c15u64;
            for i in 0..2_000_000u64 {
                x = (x ^ i).wrapping_mul(0xbf58_476d_1ce4_e5b9).rotate_left(17);
            }
            std::hint::black_box(x);
            SAMPLES.lock().unwrap().entry("cpu_probe_us").or_default().push(t.elapsed().as_secs_f64() * 1e6);
        });
        let top = conversation.read_with(cx, |c, _| c.bench_scroll_top());
        tracing::info!("bench scroll top after the pass: {top:?}");
        let frames: Vec<f64> = stamps.windows(2).map(|w| (w[1] - w[0]).as_secs_f64() * 1000.).collect();
        let span = stamps.last().zip(stamps.first()).map(|(b, a)| (*b - *a).as_secs_f64()).unwrap_or(0.);
        if let Some(ms) = open_ms {
            opens.push(ms);
        }
        intervals.extend(frames.iter().copied());
        report.push(json!({
            "conversation": format!("{conv:?}"),
            "cached_messages": cached,
            "messages_shown": shown,
            "open_ms": open_ms,
            "scroll_fps": if span > 0. { frames.len() as f64 / span } else { 0. },
            "frame_ms": summary(frames),
            "footprint_mb_after": footprint_mb(),
        }));
    }
    // Back to each conversation again: what switching between conversations costs.
    // PUZIL_BENCH_REOPENS=k repeats the pass k times (memory should level off, not climb).
    let passes: usize = std::env::var("PUZIL_BENCH_REOPENS").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let convs = store.busiest_convs(opens_only.unwrap_or(CONVERSATIONS)).unwrap_or_default();
    for (pass, (conv, _)) in std::iter::repeat_n(convs, passes).enumerate().flat_map(|(i, c)| c.into_iter().map(move |c| (i, c))) {
        if pass > 0 && std::env::var_os("PUZIL_BENCH_TRACE").is_some() {
            tracing::info!("bench reopen pass {pass}: footprint {:.1}", footprint_mb());
        }
        set_phase(Some("reopen_frame_work_ms"));
        let ws = workspace.clone();
        frame(root, cx, move |_, window, cx| ws.update(cx, |w, cx| w.open(Target::Conv(conv), window, cx))).await;
        frame(root, cx, |_, _, _| {}).await;
        set_phase(None);
        sleep(cx, 300).await;
    }
    record("final_footprint_mb", footprint_mb());
    record("cpu_seconds", cpu_seconds());
    record("final_rss_mb", rss_mb());
    // And at rest: background work (math, image decoding) has let go, and the caret has
    // stopped blinking (10 s after the last focus), so nothing redraws. macOS stops counting
    // ~48 MB of a window's GPU memory about a second after it last drew.
    set_phase(Some("settle_frame_work_ms"));
    sleep(cx, 15_000).await;
    set_phase(None);
    record("settled_footprint_mb", footprint_mb());

    let mut sorted = intervals.clone();
    sorted.sort_by(f64::total_cmp);
    // A frame "drops" when it takes more than one and a half of the display's own frame
    // (estimated as the fastest tenth of frames).
    let vsync = pct(&sorted, 0.1);
    let dropped = sorted.iter().filter(|&&f| f > vsync * 1.5).count();
    let total: f64 = sorted.iter().sum();
    record("open_ms_p50", pct(&{ let mut o = opens.clone(); o.sort_by(f64::total_cmp); o }, 0.5));
    record("open_ms_max", opens.iter().copied().fold(0., f64::max));
    record("scroll_fps", if total > 0. { sorted.len() as f64 / (total / 1000.) } else { 0. });
    record("scroll_frame_ms_p95", pct(&sorted, 0.95));
    record("scroll_frame_ms_p99", pct(&sorted, 0.99));
    record("scroll_dropped_pct", if sorted.is_empty() { 0. } else { dropped as f64 * 100. / sorted.len() as f64 });
    record("display_frame_ms", vsync);
    json!(report)
}
