//! The Dock icon. A bundled `.app` gets it from `Puzil.icns`; set here when Puzil runs as a
//! bare binary (as in development), after the first frame.

#[cfg(target_os = "macos")]
pub fn set_icon() {
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    static ICON: &[u8] = include_bytes!("../../../assets/icon/puzil-512.png");
    // A bundle's Dock icon comes from its Info.plist; decoding the PNG costs ~18 ms.
    if std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/")) {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else { return };
    let data = NSData::with_bytes(ICON);
    let image = NSImage::initWithData(NSImage::alloc(), &data);
    let app = NSApplication::sharedApplication(mtm);
    unsafe { app.setApplicationIconImage(image.as_deref()) };
}

#[cfg(not(target_os = "macos"))]
pub fn set_icon() {}
