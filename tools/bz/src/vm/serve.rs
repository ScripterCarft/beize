//! A minimal HTTP server for the guest: serves the files of one directory,
//! as a release server would. QEMU's user network forwards the guest's
//! connections to the slirp gateway to the host's loopback, so the guest
//! reaches it at `http://10.0.2.2:<port>/`.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::Result;

use super::machine::SLIRP_GATEWAY;

pub(super) struct Server {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl Server {
    /// Serves the files directly in `dir` until dropped.
    pub(super) fn start(dir: &Path) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let dir = dir.to_owned();
        thread::spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    let _ = answer(stream, &dir);
                }
            }
        });
        Ok(Self { port, stop })
    }

    /// The server's address as the guest sees it.
    pub(super) fn url(&self) -> String {
        format!("http://{SLIRP_GATEWAY}:{}/", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the accept loop so it sees the flag.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// Answers one GET request with the file it names, or 404.
fn answer(stream: TcpStream, dir: &Path) -> io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    // The headers are not needed; read them so the client is not cut off.
    let mut header = String::new();
    while reader.read_line(&mut header)? > 2 {
        header.clear();
    }
    let mut stream = reader.into_inner();
    match file_of(&request, dir).and_then(|path| File::open(path).ok()) {
        Some(mut file) => {
            let len = file.metadata()?.len();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
            )?;
            io::copy(&mut file, &mut stream)?;
        }
        None => write!(
            stream,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?,
    }
    stream.flush()
}

/// The file a request line names: a plain name in `dir`, nothing else.
fn file_of(request: &str, dir: &Path) -> Option<PathBuf> {
    let name = request.strip_prefix("GET /")?.split(' ').next()?;
    let plain = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !name.starts_with('.');
    plain.then(|| dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn serves_plain_names_only() {
        let dir = Path::new("/srv");
        assert_eq!(
            file_of("GET /SHA256SUMS HTTP/1.1\r\n", dir),
            Some(dir.join("SHA256SUMS"))
        );
        for bad in [
            "GET /../x HTTP/1.1",
            "GET / HTTP/1.1",
            "GET /a/b HTTP/1.1",
            "PUT /x HTTP/1.1",
        ] {
            assert_eq!(file_of(bad, dir), None, "{bad}");
        }
    }

    #[test]
    fn answers_a_request() {
        let dir = std::env::temp_dir().join(format!("bz-serve-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file"), "content").unwrap();
        let server = Server::start(&dir).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        stream
            .write_all(b"GET /file HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"), "{reply}");
        assert!(reply.ends_with("\r\n\r\ncontent"), "{reply}");
        drop(server);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
