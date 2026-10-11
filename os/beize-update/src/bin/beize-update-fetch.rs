//! `beize-update-fetch` – fetches a release for beize-updated (ADR 0020).
//! One process per request (beize-update-fetch.socket, Accept=yes), as a
//! user without privileges (DynamicUser): it can reach the network and write
//! its cache, nothing else.
//!
//! - `Fetch` source → version: the list and its signature first, checked
//!   with the update key and against the running version; only then the
//!   three files, each no larger than its slot and matching its sum.
//! - `Clear`: empties the cache once the updater has its copy.
//!
//! A source is a directory (a USB stick, a share) or an http(s):// URL.

use std::ffi::{CStr, CString, c_char, c_int, c_long, c_void};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use beize_update::list::{MAX_LIST, MAX_SIGNATURE};
use beize_update::{
    CACHE, FETCH_INTERFACE, LIST, Refused, SIGNATURE, check_list, crypto, error, read_limited,
    version_reply,
};
use beize_varlink::json::Object;
use sha2::{Digest, Sha256};

fn main() -> ExitCode {
    beize_update::serve(FETCH_INTERFACE, |method, params, _uid| match method {
        "Fetch" => fetch(params),
        "Clear" => clear().map(|()| String::new()),
        _ => Err(Refused::failed(format!("no method {method:?}"))),
    })
}

/// Where a release comes from.
enum Source {
    Directory(String),
    Url(String),
}

impl Source {
    fn parse(text: &str) -> Result<Self, Refused> {
        if text.starts_with("https://") || text.starts_with("http://") {
            Ok(Self::Url(text.trim_end_matches('/').to_owned()))
        } else if text.starts_with('/') {
            Ok(Self::Directory(text.to_owned()))
        } else {
            Err(Refused::failed(format!(
                "{text:?} is neither a directory nor an http(s):// URL"
            )))
        }
    }

    /// Reads `name` from the source, at most `max` bytes, into `out`.
    fn get(&self, name: &str, max: u64, out: &mut dyn Write) -> Result<(), Refused> {
        match self {
            Self::Directory(dir) => {
                let path = Path::new(dir).join(name);
                let file = File::open(&path)
                    .map_err(|e| Refused::failed(format!("{}: {e}", path.display())))?;
                if file.metadata().map(|m| m.len()).unwrap_or(0) > max {
                    return Err(Refused(
                        error::UNTRUSTED,
                        format!("{name} is larger than allowed"),
                    ));
                }
                let mut limited = std::io::Read::take(file, max);
                std::io::copy(&mut limited, out)
                    .map(drop)
                    .map_err(|e| Refused::failed(format!("{}: {e}", path.display())))
            }
            Self::Url(base) => curl::download(&format!("{base}/{name}"), max, out),
        }
    }

    fn get_small(&self, name: &str, max: u64) -> Result<Vec<u8>, Refused> {
        let mut data = Vec::new();
        self.get(name, max, &mut data)?;
        read_limited(&data[..], max, name).map_err(|e| Refused(error::UNTRUSTED, e))
    }
}

fn fetch(params: Object) -> Result<String, Refused> {
    let source = Source::parse(params.str("source").unwrap_or(""))?;
    clear()?;
    let list = source.get_small(LIST, MAX_LIST)?;
    let signature = source.get_small(SIGNATURE, MAX_SIGNATURE)?;
    let release = check_list(&list, &signature)?;
    let cache = Path::new(CACHE);
    let write = |name: &str, data: &[u8]| {
        fs::write(cache.join(name), data).map_err(|e| Refused::failed(format!("{name}: {e}")))
    };
    write(LIST, &list)?;
    write(SIGNATURE, &signature)?;
    for entry in &release.entries {
        let path = cache.join(&entry.name);
        let file =
            File::create(&path).map_err(|e| Refused::failed(format!("{}: {e}", entry.name)))?;
        let mut hashed = Hashed {
            file,
            digest: Sha256::new(),
        };
        source.get(&entry.name, entry.kind.max_size(), &mut hashed)?;
        hashed
            .file
            .sync_all()
            .map_err(|e| Refused::failed(format!("{}: {e}", entry.name)))?;
        if crypto::hex(hashed.digest) != entry.sha256 {
            let _ = fs::remove_file(&path);
            return Err(Refused(
                error::UNTRUSTED,
                format!("{} does not match the list", entry.name),
            ));
        }
    }
    Ok(version_reply(&release.version))
}

/// A file that hashes what is written to it.
struct Hashed {
    file: File,
    digest: Sha256,
}

impl Write for Hashed {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let written = self.file.write(data)?;
        self.digest.update(&data[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

fn clear() -> Result<(), Refused> {
    let entries = fs::read_dir(CACHE).map_err(|e| Refused::failed(format!("{CACHE}: {e}")))?;
    for entry in entries {
        let path = entry.map_err(|e| Refused::failed(e.to_string()))?.path();
        fs::remove_file(&path).map_err(|e| Refused::failed(format!("{}: {e}", path.display())))?;
    }
    Ok(())
}

/// Downloads with libcurl: HTTP(S) only, redirects only to HTTP(S), TLS
/// checked against the system's trust store (ca-certificates).
mod curl {
    use super::*;

    const CURLOPT_WRITEDATA: c_int = 10001;
    const CURLOPT_URL: c_int = 10002;
    const CURLOPT_USERAGENT: c_int = 10018;
    const CURLOPT_PROTOCOLS_STR: c_int = 10318;
    const CURLOPT_REDIR_PROTOCOLS_STR: c_int = 10319;
    const CURLOPT_WRITEFUNCTION: c_int = 20011;
    const CURLOPT_MAXFILESIZE_LARGE: c_int = 30117;
    const CURLOPT_LOW_SPEED_LIMIT: c_int = 19;
    const CURLOPT_LOW_SPEED_TIME: c_int = 20;
    const CURLOPT_FAILONERROR: c_int = 45;
    const CURLOPT_FOLLOWLOCATION: c_int = 52;
    const CURLOPT_MAXREDIRS: c_int = 68;
    const CURLOPT_CONNECTTIMEOUT: c_int = 78;
    const CURLOPT_NOSIGNAL: c_int = 99;
    const CURL_GLOBAL_DEFAULT: c_long = 3;
    /// The write callback refused more data (CURLE_WRITE_ERROR).
    const CURLE_WRITE_ERROR: c_int = 23;

    #[link(name = "curl")]
    unsafe extern "C" {
        fn curl_global_init(flags: c_long) -> c_int;
        fn curl_easy_init() -> *mut c_void;
        fn curl_easy_setopt(handle: *mut c_void, option: c_int, ...) -> c_int;
        fn curl_easy_perform(handle: *mut c_void) -> c_int;
        fn curl_easy_cleanup(handle: *mut c_void);
        fn curl_easy_strerror(code: c_int) -> *const c_char;
    }

    /// Where the callback writes, and how much it still may.
    struct Sink<'a> {
        out: &'a mut dyn Write,
        left: u64,
        failed: Option<String>,
    }

    extern "C" fn write(
        data: *const c_char,
        size: usize,
        count: usize,
        sink: *mut c_void,
    ) -> usize {
        let len = size * count;
        // SAFETY: `sink` is the Sink passed as CURLOPT_WRITEDATA, alive for
        // the whole transfer; `data` holds `len` bytes.
        let (sink, data) = unsafe {
            (
                &mut *sink.cast::<Sink>(),
                std::slice::from_raw_parts(data.cast::<u8>(), len),
            )
        };
        if len as u64 > sink.left {
            sink.failed = Some("larger than allowed".into());
            return 0;
        }
        match sink.out.write_all(data) {
            Ok(()) => {
                sink.left -= len as u64;
                len
            }
            Err(e) => {
                sink.failed = Some(e.to_string());
                0
            }
        }
    }

    pub fn download(url: &str, max: u64, out: &mut dyn Write) -> Result<(), Refused> {
        let c_url = CString::new(url).map_err(|_| Refused::failed("a NUL byte in the URL"))?;
        let mut sink = Sink {
            out,
            left: max,
            failed: None,
        };
        // SAFETY: the handle is checked for null and cleaned up once; every
        // string and the sink outlive curl_easy_perform; option values have
        // the types libcurl documents (long, char *, off_t, callback).
        let code = unsafe {
            if curl_global_init(CURL_GLOBAL_DEFAULT) != 0 {
                return Err(Refused::failed("libcurl did not start"));
            }
            let h = curl_easy_init();
            if h.is_null() {
                return Err(Refused::failed("libcurl did not start"));
            }
            let options: [(c_int, c_long); 7] = [
                (CURLOPT_NOSIGNAL, 1),
                (CURLOPT_FAILONERROR, 1),
                (CURLOPT_FOLLOWLOCATION, 1),
                (CURLOPT_MAXREDIRS, 5),
                (CURLOPT_CONNECTTIMEOUT, 30),
                // Give up when less than 1 KiB/s arrives for a minute.
                (CURLOPT_LOW_SPEED_LIMIT, 1024),
                (CURLOPT_LOW_SPEED_TIME, 60),
            ];
            let mut ok = options
                .iter()
                .all(|&(option, value)| curl_easy_setopt(h, option, value) == 0);
            for (option, value) in [
                (CURLOPT_URL, c_url.as_ptr()),
                (CURLOPT_PROTOCOLS_STR, c"http,https".as_ptr()),
                (CURLOPT_REDIR_PROTOCOLS_STR, c"http,https".as_ptr()),
                (CURLOPT_USERAGENT, c"beize-update".as_ptr()),
            ] {
                ok &= curl_easy_setopt(h, option, value) == 0;
            }
            ok &= curl_easy_setopt(h, CURLOPT_MAXFILESIZE_LARGE, max as i64) == 0;
            ok &= curl_easy_setopt(
                h,
                CURLOPT_WRITEFUNCTION,
                write as extern "C" fn(*const c_char, usize, usize, *mut c_void) -> usize,
            ) == 0;
            ok &= curl_easy_setopt(h, CURLOPT_WRITEDATA, (&raw mut sink).cast::<c_void>()) == 0;
            let code = if ok { curl_easy_perform(h) } else { -1 };
            curl_easy_cleanup(h);
            code
        };
        match code {
            0 => Ok(()),
            -1 => Err(Refused::failed("libcurl refused an option")),
            CURLE_WRITE_ERROR if sink.failed.is_some() => Err(Refused(
                error::UNTRUSTED,
                format!("{url}: {}", sink.failed.unwrap_or_default()),
            )),
            code => {
                // SAFETY: libcurl returns a static string for every code.
                let text = unsafe { CStr::from_ptr(curl_easy_strerror(code)) };
                Err(Refused::failed(format!(
                    "{url}: {}",
                    text.to_string_lossy()
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_directories_and_http_urls_only() {
        assert!(matches!(
            Source::parse("/run/media/stick/"),
            Ok(Source::Directory(_))
        ));
        assert!(
            matches!(Source::parse("https://example.org/beize/"), Ok(Source::Url(u)) if u == "https://example.org/beize")
        );
        for bad in ["ftp://x/", "file:///x", "relative/dir", ""] {
            assert!(Source::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_directory_source_keeps_the_limit() {
        let dir = std::env::temp_dir().join(format!("beize-fetch-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("big"), vec![0u8; 100]).unwrap();
        let source = Source::Directory(dir.display().to_string());
        let mut out = Vec::new();
        assert!(source.get("big", 99, &mut out).is_err());
        assert!(source.get("big", 100, &mut out).is_ok());
        assert_eq!(out.len(), 100);
        fs::remove_dir_all(&dir).unwrap();
    }
}
