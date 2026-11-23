//! The Dock icon. A bundled `.app` gets it from `Puzil.icns`; set here too so it shows when
//! Puzil runs as a bare binary (as in development).

#[cfg(target_os = "macos")]
pub fn set_icon() {
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    static ICON: &[u8] = include_bytes!("../../../assets/icon/puzil-512.png");
    let Some(mtm) = MainThreadMarker::new() else { return };
    let data = NSData::with_bytes(ICON);
    let image = NSImage::initWithData(NSImage::alloc(), &data);
    let app = NSApplication::sharedApplication(mtm);
    unsafe { app.setApplicationIconImage(image.as_deref()) };
}

#[cfg(not(target_os = "macos"))]
pub fn set_icon() {}
