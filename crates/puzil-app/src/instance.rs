//! Single-instance handoff for Linux, where the desktop launches a new process per
//! `zulip://` URL. The first instance listens on `$XDG_RUNTIME_DIR/puzil.sock`;
//! later ones send their URLs there and exit. A socket that refuses connections is stale
//! (the owner crashed) and is replaced. On macOS this is unused: LaunchServices delivers
//! URLs to the running app through `application:openURLs:`.

pub type Incoming = Option<std::sync::mpsc::Receiver<String>>;

#[cfg(target_os = "linux")]
fn socket_path() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(Into::into).unwrap_or_else(std::env::temp_dir);
    std::path::Path::new(&dir).join("puzil.sock")
}

/// Returns `None` when the URLs were forwarded and this process should exit.
#[cfg(target_os = "linux")]
pub fn claim_or_forward(urls: &[String]) -> Option<Incoming> {
    use std::{io::Write, os::unix::net::{UnixListener, UnixStream}};
    let path = socket_path();
    if let Ok(mut s) = UnixStream::connect(&path) {
        for u in urls {
            let _ = writeln!(s, "{u}");
        }
        eprintln!("[puzil] forwarded {} URL(s) to the running instance", urls.len());
        return None;
    }
    let _ = std::fs::remove_file(&path); // stale
    let listener = UnixListener::bind(&path).expect("bind instance socket");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for conn in listener.incoming().flatten() {
            for line in std::io::BufReader::new(conn).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        }
    });
    Some(Some(rx))
}

#[cfg(not(target_os = "linux"))]
pub fn claim_or_forward(_: &[String]) -> Option<Incoming> {
    Some(None)
}

pub fn pump(incoming: Incoming, tx: tokio::sync::mpsc::UnboundedSender<String>) {
    if let Some(rx) = incoming {
        std::thread::spawn(move || {
            for u in rx {
                let _ = tx.send(u);
            }
        });
    }
}
