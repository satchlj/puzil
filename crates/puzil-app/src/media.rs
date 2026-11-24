//! Images from the server: fetched with the account's credentials, cached on disk by URL,
//! decoded off the main thread at the size they're shown ([`Fit`]), and kept in memory up to
//! a cap.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::*;
use puzil_zulip::Client;

use crate::runtime::on_tokio;

/// Bytes of decoded pixels kept in memory; the oldest images are dropped past this (elements
/// still showing them keep their own reference, and the disk cache makes reloads cheap).
const MAX_DECODED_BYTES: usize = 64 << 20;

/// A failed image is asked for again after this (a network blip at launch shouldn't leave
/// broken images until restart).
const RETRY_FAILED: Duration = Duration::from_secs(60);

/// The disk cache is trimmed, oldest files first, to `CACHE_KEEP` at launch once it passes
/// `CACHE_MAX`.
const CACHE_MAX: u64 = 512 << 20;
const CACHE_KEEP: u64 = 384 << 20;

/// Frames kept of an animated GIF; longer ones show their first `MAX_FRAMES`.
const MAX_FRAMES: usize = 240;

/// How large an image is kept once decoded: no bigger than it's drawn on a 2× display. The
/// original stays on disk, so the lightbox can still decode it in full.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fit {
    /// Avatars and organization icons: the largest drawn is 80 points (the profile card).
    Avatar,
    /// Images in messages, shown at most 480×320 points, and custom emoji.
    Inline,
    /// The lightbox: the original.
    Full,
}

impl Fit {
    fn max(self) -> Option<(u32, u32)> {
        match self {
            Fit::Avatar => Some((192, 192)),
            Fit::Inline => Some((960, 640)),
            Fit::Full => None,
        }
    }
}

#[derive(Clone)]
pub enum ImgState {
    Loading,
    Ready(Arc<RenderImage>),
    Failed,
}

/// URLs whose images just finished loading (or failed).
pub struct ImagesReady(pub Rc<HashSet<String>>);

pub struct Images {
    client: Client,
    dir: PathBuf,
    map: HashMap<(Fit, String), ImgState>,
    /// Loaded images, oldest first, with their decoded size in bytes.
    order: VecDeque<((Fit, String), usize)>,
    decoded_bytes: usize,
    ready: HashSet<String>,
    flush: Option<Task<()>>,
    /// URLs whose image is a generated identicon (Zulip's default avatar).
    defaults: HashSet<String>,
    /// Our own generated avatars by user id; `None` while one renders.
    generated: HashMap<(u64, bool), Option<Arc<RenderImage>>>,
    /// When each failed image failed.
    failed_at: HashMap<(Fit, String), Instant>,
}

impl EventEmitter<ImagesReady> for Images {}

thread_local! {
    /// When each image (by allocation) became ready, for fading in only what just arrived.
    static READY_AT: std::cell::RefCell<HashMap<usize, std::time::Instant>> = std::cell::RefCell::new(HashMap::new());
}

fn mark_ready(img: &Arc<RenderImage>) {
    let now = std::time::Instant::now();
    READY_AT.with(|m| {
        let mut m = m.borrow_mut();
        m.retain(|_, t| now.duration_since(*t) < Duration::from_secs(2));
        m.insert(Arc::as_ptr(img) as usize, now);
    });
}

/// Whether `img` finished loading a moment ago (so showing it should fade in; an image
/// already on screen before, scrolled back into view, should just appear).
pub fn just_arrived(img: &Arc<RenderImage>) -> bool {
    READY_AT.with(|m| m.borrow().get(&(Arc::as_ptr(img) as usize)).is_some_and(|t| t.elapsed() < Duration::from_millis(250)))
}

fn image_bytes(img: &RenderImage) -> usize {
    (0..img.frame_count()).filter_map(|i| img.as_bytes(i)).map(<[u8]>::len).sum()
}

impl Images {
    pub fn new(client: Client, dir: PathBuf) -> Self {
        let d = dir.clone();
        std::thread::Builder::new().name("image-cache-trim".into()).spawn(move || trim_cache(&d)).ok();
        let defaults =
            std::fs::read_to_string(dir.join("defaults.json")).ok().and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
        Self {
            client,
            dir,
            map: HashMap::new(),
            order: VecDeque::new(),
            decoded_bytes: 0,
            ready: HashSet::new(),
            flush: None,
            defaults,
            generated: HashMap::new(),
            failed_at: HashMap::new(),
        }
    }

    /// The image if it's in memory; starts loading it otherwise.
    #[allow(dead_code)]
    pub fn ready(&mut self, url: &str, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        match self.get_fit(url, Fit::Inline, cx) {
            ImgState::Ready(i) => Some(i),
            _ => None,
        }
    }

    /// An image in a message (or a custom emoji), at inline size.
    pub fn get(&mut self, url: &str, cx: &mut Context<Self>) -> ImgState {
        self.get_fit(url, Fit::Inline, cx)
    }

    pub fn get_fit(&mut self, url: &str, fit: Fit, cx: &mut Context<Self>) -> ImgState {
        let key = (fit, url.to_string());
        match self.map.get(&key) {
            Some(ImgState::Failed) if self.failed_at.get(&key).is_some_and(|t| t.elapsed() >= RETRY_FAILED) => {
                self.failed_at.remove(&key);
                // Its old place in the eviction order goes with it.
                if let Some(i) = self.order.iter().position(|(k, _)| *k == key)
                    && let Some((_, n)) = self.order.remove(i)
                {
                    self.decoded_bytes -= n;
                }
            }
            Some(s) => return s.clone(),
            None => {}
        }
        self.map.insert(key, ImgState::Loading);
        self.load(url.to_string(), fit, cx);
        ImgState::Loading
    }

    /// Drops a full-size image (the lightbox closed); elements still drawing it keep theirs.
    pub fn forget_full(&mut self, url: &str) {
        let key = (Fit::Full, url.to_string());
        if self.map.remove(&key).is_some()
            && let Some(i) = self.order.iter().position(|(k, _)| *k == key)
            && let Some((_, n)) = self.order.remove(i)
        {
            self.decoded_bytes -= n;
        }
    }

    /// A user's avatar: their uploaded picture, or our generated one when they have none or
    /// Zulip gave them its default identicon. `None` until either is ready.
    pub fn avatar(&mut self, user: u64, url: Option<&str>, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        self.picture(user, url, true, cx)
    }

    /// The organization's icon, or a generated square one in place of its default.
    pub fn realm_icon(&mut self, realm: &str, url: Option<&str>, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        let seed = u64::from_le_bytes(blake3::hash(realm.as_bytes()).as_bytes()[..8].try_into().unwrap_or_default());
        self.picture(seed, url, false, cx)
    }

    fn picture(&mut self, key: u64, url: Option<&str>, round: bool, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        // A picture known to be Zulip's default goes straight to ours, without loading it.
        if let Some(u) = url
            && !self.defaults.contains(u)
        {
            match self.get_fit(u, Fit::Avatar, cx) {
                ImgState::Ready(img) if !self.defaults.contains(u) => return Some(img),
                ImgState::Loading => return None,
                _ => {}
            }
        }
        if let Some(g) = self.generated.get(&(key, round)) {
            return g.clone();
        }
        self.generated.insert((key, round), None);
        cx.spawn(async move |this, cx| {
            let img = cx
                .background_spawn(async move {
                    let mut rgba = puzil_avatar::Style::for_user(key).render_shaped(128, round);
                    for px in rgba.chunks_exact_mut(4) {
                        px.swap(0, 2);
                    }
                    Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(rgba)]))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                mark_ready(&img);
                this.generated.insert((key, round), Some(img));
                cx.notify();
            });
        })
        .detach();
        None
    }

    /// An inline-size image, if it's in memory.
    pub fn peek(&self, url: &str) -> Option<Arc<RenderImage>> {
        self.peek_fit(url, Fit::Inline)
    }

    pub fn peek_fit(&self, url: &str, fit: Fit) -> Option<Arc<RenderImage>> {
        match self.map.get(&(fit, url.to_string())) {
            Some(ImgState::Ready(i)) => Some(i.clone()),
            _ => None,
        }
    }

    fn cache_path(&self, url: &str) -> PathBuf {
        self.dir.join(blake3::hash(url.as_bytes()).to_hex().as_str())
    }

    fn load(&mut self, url: String, fit: Fit, cx: &mut Context<Self>) {
        let path = self.cache_path(&url);
        let client = self.client.clone();
        let fetch_url = url.clone();
        cx.spawn(async move |this, cx| {
            let cached = cx.background_spawn({
                let path = path.clone();
                async move { std::fs::read(&path).ok() }
            });
            let bytes = match cached.await {
                Some(b) => Ok(b),
                None => {
                    let r = on_tokio(async move { client.get_bytes(&fetch_url).await }).await;
                    if let Ok(b) = &r {
                        let (b, path) = (b.clone(), path.clone());
                        cx.background_spawn(async move {
                            if let Some(d) = path.parent() {
                                let _ = std::fs::create_dir_all(d);
                            }
                            let _ = std::fs::write(&path, b);
                        })
                        .detach();
                    }
                    r.map_err(|e| e.to_string())
                }
            };
            let decoded = match bytes {
                Ok(b) => cx.background_spawn(async move { decode(&b, fit.max()) }).await,
                Err(e) => Err(e),
            };
            let _ = this.update(cx, |this, cx| {
                let state = match decoded {
                    Ok((img, generated, flat)) => {
                        // Gravatar's identicon (asked for with `d=identicon`, as Zulip does
                        // for organizations without an icon) is flat but not quite symmetric.
                        let generated = generated || (url.contains("gravatar.com/") && url.contains("d=identicon") && flat > 0.85);
                        if generated && this.defaults.insert(url.clone()) {
                            // Remembered, so the next launch skips loading it.
                            if let Ok(j) = serde_json::to_string(&this.defaults) {
                                let path = this.dir.join("defaults.json");
                                cx.background_spawn(async move { std::fs::write(path, j) }).detach();
                            }
                        }
                        let img = Arc::new(img);
                        mark_ready(&img);
                        ImgState::Ready(img)
                    }
                    Err(e) => {
                        tracing::debug!("image {url}: {e}");
                        ImgState::Failed
                    }
                };
                let size = match &state {
                    ImgState::Ready(i) => image_bytes(i),
                    _ => 0,
                };
                let key = (fit, url.clone());
                if !this.map.contains_key(&key) {
                    // Forgotten while it loaded (the lightbox closed).
                    return;
                }
                if matches!(state, ImgState::Failed) {
                    this.failed_at.insert(key.clone(), Instant::now());
                }
                this.map.insert(key.clone(), state);
                this.order.push_back((key, size));
                this.decoded_bytes += size;
                while this.decoded_bytes > MAX_DECODED_BYTES && this.order.len() > 1 {
                    if let Some((old, n)) = this.order.pop_front() {
                        this.map.remove(&old);
                        this.decoded_bytes -= n;
                    }
                }
                this.ready.insert(url);
                // Batch arrivals so a screenful of images rebuilds each message once.
                if this.flush.is_none() {
                    this.flush = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(30)).await;
                        let _ = this.update(cx, |this, cx| {
                            this.flush = None;
                            let urls = std::mem::take(&mut this.ready);
                            cx.emit(ImagesReady(Rc::new(urls)));
                            cx.notify();
                        });
                    }));
                }
            });
        })
        .detach();
    }
}

/// Bytes → BGRA frames no larger than `max` (aspect kept, never enlarged), whether the image
/// is a generated identicon, and how flat its colours are (see `puzil_avatar::flatness`),
/// both judged on the original. GIFs keep their animation frames.
fn decode(bytes: &[u8], max: Option<(u32, u32)>) -> Result<(RenderImage, bool, f32), String> {
    use image::AnimationDecoder;
    let fmt = image::guess_format(bytes).map_err(|e| e.to_string())?;
    // Each frame is shrunk to fit as it's decoded, so a long GIF never holds all its frames
    // at full size at once.
    let shrink = |f: image::Frame| -> image::Frame {
        let Some((mw, mh)) = max else { return f };
        let (w, h) = f.buffer().dimensions();
        if w <= mw && h <= mh {
            return f;
        }
        let scale = (mw as f64 / w as f64).min(mh as f64 / h as f64);
        let (nw, nh) = (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1));
        let (left, top, delay) = (f.left(), f.top(), f.delay());
        let small = image::imageops::resize(f.buffer(), nw, nh, image::imageops::FilterType::CatmullRom);
        image::Frame::from_parts(small, left, top, delay)
    };
    let mut frames: smallvec::SmallVec<[image::Frame; 1]> = smallvec::SmallVec::new();
    // Judged on the original (first) frame.
    let mut judged = None;
    if fmt == image::ImageFormat::Gif {
        let dec = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
        for f in dec.into_frames().flatten().take(MAX_FRAMES) {
            if judged.is_none() {
                judged = Some((puzil_avatar::looks_generated(f.buffer()), puzil_avatar::flatness(f.buffer())));
            }
            frames.push(shrink(f));
        }
    }
    if frames.is_empty() {
        let img = image::load_from_memory_with_format(bytes, fmt).map_err(|e| e.to_string())?.into_rgba8();
        judged = Some((puzil_avatar::looks_generated(&img), puzil_avatar::flatness(&img)));
        frames.push(shrink(image::Frame::new(img)));
    }
    let (generated, flat) = match judged {
        Some((g, f)) if frames.len() == 1 => (g, f),
        _ => (false, 0.),
    };
    for f in frames.iter_mut() {
        for px in f.buffer_mut().chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    }
    Ok((RenderImage::new(frames), generated, flat))
}

/// Deletes the oldest cached images once the cache passes `CACHE_MAX`, down to `CACHE_KEEP`.
fn trim_cache(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_name() != "defaults.json")
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= CACHE_MAX {
        return;
    }
    files.sort_by_key(|f| f.0);
    for (_, len, path) in files {
        if total <= CACHE_KEEP {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
    tracing::info!("image cache trimmed to {} MB", total >> 20);
}

/// Display size for an image of `dims` in a box of at most `max_w` × `max_h`.
pub fn fit(dims: (u32, u32), max_w: f32, max_h: f32) -> (f32, f32) {
    let (w, h) = (dims.0.max(1) as f32, dims.1.max(1) as f32);
    let s = (max_w / w).min(max_h / h).min(1.);
    ((w * s).round(), (h * s).round())
}

#[cfg(test)]
mod tests {
    #[test]
    fn fits_and_never_upscales() {
        assert_eq!(super::fit((2800, 1080), 480., 320.), (480., 185.));
        assert_eq!(super::fit((1080, 2410), 480., 320.), (143., 320.));
        assert_eq!(super::fit((100, 50), 480., 320.), (100., 50.));
    }

    #[test]
    fn decodes_png_to_bgra() {
        let mut png = Vec::new();
        let img = image::RgbaImage::from_pixel(2, 1, image::Rgba([255, 0, 0, 255]));
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let (r, generated, _) = super::decode(&png, None).unwrap();
        assert_eq!(&r.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255], "red becomes BGRA");
        assert!(!generated, "too small to judge as an identicon");
    }

    #[test]
    fn shrinks_to_fit_and_never_enlarges() {
        let png = |w, h| {
            let mut out = Vec::new();
            image::RgbaImage::from_pixel(w, h, image::Rgba([9, 9, 9, 255])).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
            out
        };
        let size = |r: &gpui_kit::RenderImage| (r.size(0).width.0, r.size(0).height.0);
        assert_eq!(size(&super::decode(&png(2600, 2000), super::Fit::Inline.max()).unwrap().0), (832, 640));
        assert_eq!(size(&super::decode(&png(500, 500), super::Fit::Avatar.max()).unwrap().0), (192, 192));
        assert_eq!(size(&super::decode(&png(100, 50), super::Fit::Inline.max()).unwrap().0), (100, 50));
        assert_eq!(size(&super::decode(&png(2600, 2000), super::Fit::Full.max()).unwrap().0), (2600, 2000));
    }
}
