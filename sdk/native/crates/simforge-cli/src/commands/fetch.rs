//! Streamed, ranged, resumable, sha256-checked downloads of signed URLs.
//!
//! Bytes go straight from the socket to a `.<name>.part` file beside the
//! destination; nothing is held in memory. An interrupted download resumes
//! from the part file with an HTTP `Range` request, a signed URL that expired
//! mid-download is renewed through the caller, and the file is renamed into
//! place only after its size and sha256 match what the API declared. A file
//! already present with the declared digest is not fetched again.

use crate::contract::CliError;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

const CHUNK: usize = 1 << 20;
const ATTEMPTS: u32 = 6;

/// What the API declared about one file.
pub struct Target<'a> {
    pub path: &'a Path,
    pub sha256: &'a str,
    pub size: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    /// Already on disk with the declared size and digest.
    Present,
    /// Downloaded now; `resumed_from` bytes came from an earlier part file.
    Downloaded { resumed_from: u64 },
}

fn io(code: &str, path: &Path, e: impl std::fmt::Display) -> CliError {
    CliError::new(code, format!("{}: {e}", path.display()))
}

fn part_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.part"))
}

/// sha256 and length of a file, read in chunks.
pub fn digest_file(path: &Path) -> Result<(String, u64), CliError> {
    let mut f = fs::File::open(path).map_err(|e| io("file_unreadable", path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut n = 0u64;
    loop {
        let k = f
            .read(&mut buf)
            .map_err(|e| io("file_unreadable", path, e))?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((format!("{:x}", h.finalize()), n))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .user_agent(concat!("simforge/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

enum Step {
    /// Keep the bytes written so far and try again (network error, 5xx).
    Retry(String),
    /// The signed URL was refused (expired): ask the caller for a new one.
    Renew(String),
    Done,
}

/// Download `target` from the URL `url()` returns, resuming a part file.
/// `url` is called again whenever the signed URL is refused, so callers pass
/// a closure that asks the API for a fresh one. `progress` gets every byte
/// count written.
pub fn fetch(
    url: &mut dyn FnMut() -> Result<String, CliError>,
    target: &Target,
    progress: &mut dyn FnMut(u64),
) -> Result<Fetched, CliError> {
    if let Ok(meta) = fs::metadata(target.path) {
        if meta.is_file() && meta.len() == target.size {
            let (sha, _) = digest_file(target.path)?;
            if sha == target.sha256 {
                progress(target.size);
                return Ok(Fetched::Present);
            }
        }
    }
    if let Some(parent) = target.path.parent() {
        fs::create_dir_all(parent).map_err(|e| io("file_unwritable", parent, e))?;
    }
    let part = part_path(target.path);
    let mut hasher = Sha256::new();
    let mut offset = match fs::metadata(&part) {
        Ok(m) if m.len() <= target.size => {
            let mut f = fs::File::open(&part).map_err(|e| io("file_unreadable", &part, e))?;
            let mut buf = vec![0u8; CHUNK];
            loop {
                let k = f
                    .read(&mut buf)
                    .map_err(|e| io("file_unreadable", &part, e))?;
                if k == 0 {
                    break;
                }
                hasher.update(&buf[..k]);
            }
            m.len()
        }
        _ => {
            let _ = fs::remove_file(&part); // fallback-ok: an oversized part file is restarted from zero
            0
        }
    };
    let resumed_from = offset;
    progress(offset);
    let mut href = url()?;
    let mut last = String::new();
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(500 * (1 << attempt.min(5))));
        }
        match step(
            &href,
            &part,
            target.size,
            &mut offset,
            &mut hasher,
            progress,
        )? {
            Step::Done => {
                let got = format!("{:x}", hasher.finalize());
                if got != target.sha256 {
                    let _ = fs::remove_file(&part); // fallback-ok: corrupt bytes are never kept
                    return Err(CliError::findings(
                        "artifact_checksum_mismatch",
                        format!(
                            "{}: expected sha256 {}, got {got}",
                            target.path.display(),
                            target.sha256
                        ),
                    ));
                }
                fs::rename(&part, target.path)
                    .map_err(|e| io("file_unwritable", target.path, e))?;
                return Ok(Fetched::Downloaded { resumed_from });
            }
            Step::Retry(why) => last = why,
            Step::Renew(why) => {
                last = why;
                href = url()?;
            }
        }
    }
    Err(CliError::new(
        "download_failed",
        format!(
            "{}: {last} (after {ATTEMPTS} attempts; run the command again to resume)",
            target.path.display()
        ),
    ))
}

fn step(
    href: &str,
    part: &Path,
    size: u64,
    offset: &mut u64,
    hasher: &mut Sha256,
    progress: &mut dyn FnMut(u64),
) -> Result<Step, CliError> {
    if *offset == size {
        return Ok(Step::Done);
    }
    let mut req = agent().get(href);
    if *offset > 0 {
        req = req.header("Range", format!("bytes={}-", *offset));
    }
    let resp = match req.call() {
        Ok(r) => r,
        Err(e) => return Ok(Step::Retry(transport_reason(&e))),
    };
    let status = resp.status().as_u16();
    match status {
        206 => {
            let start = resp
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("bytes "))
                .and_then(|v| v.split('-').next())
                .and_then(|v| v.parse::<u64>().ok());
            if start != Some(*offset) {
                return Err(CliError::new(
                    "download_range_mismatch",
                    format!(
                        "asked for bytes from {} and the host answered {start:?}",
                        *offset
                    ),
                ));
            }
        }
        200 => {
            if *offset > 0 {
                // The host ignored the range: start over rather than splice.
                *hasher = Sha256::new();
                *offset = 0;
                fs::File::create(part).map_err(|e| io("file_unwritable", part, e))?;
            }
        }
        403 | 401 | 400 => return Ok(Step::Renew(format!("HTTP {status}"))),
        s if s >= 500 || s == 429 => return Ok(Step::Retry(format!("HTTP {s}"))),
        s => {
            return Err(CliError::new("download_failed", format!("HTTP {s}")));
        }
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(part)
        .map_err(|e| io("file_unwritable", part, e))?;
    let mut reader = resp.into_body().into_reader();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let k = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(k) => k,
            Err(e) => {
                file.sync_all()
                    .map_err(|e| io("file_unwritable", part, e))?;
                return Ok(Step::Retry(format!("body read failed ({:?})", e.kind())));
            }
        };
        if *offset + k as u64 > size {
            let _ = fs::remove_file(part); // fallback-ok: an over-long body is never kept
            return Err(CliError::new(
                "download_size_mismatch",
                format!("the host sent more than the declared {size} bytes"),
            ));
        }
        file.write_all(&buf[..k])
            .map_err(|e| io("file_unwritable", part, e))?;
        hasher.update(&buf[..k]);
        *offset += k as u64;
        progress(k as u64);
    }
    file.sync_all()
        .map_err(|e| io("file_unwritable", part, e))?;
    if *offset < size {
        return Ok(Step::Retry(format!(
            "connection closed at {} of {size} bytes",
            *offset
        )));
    }
    Ok(Step::Done)
}

fn transport_reason(error: &ureq::Error) -> String {
    let category = match error {
        ureq::Error::StatusCode(status) => return format!("HTTP {status}"),
        ureq::Error::BadUri(_) | ureq::Error::Http(_) => "invalid_uri_or_header",
        ureq::Error::HostNotFound => "dns_failed",
        ureq::Error::Timeout(_) => "timeout",
        ureq::Error::RedirectFailed | ureq::Error::TooManyRedirects => "redirect_failed",
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) | ureq::Error::TlsRequired => "tls_failed",
        ureq::Error::Io(error) => return format!("transport I/O failed ({:?})", error.kind()),
        ureq::Error::Protocol(_) => "http_protocol_failed",
        ureq::Error::InvalidProxyUrl | ureq::Error::ConnectProxyFailed(_) => "proxy_failed",
        _ => "transport_failed",
    };
    format!("signed download transport failed ({category})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        sync::{Arc, Mutex},
    };

    /// A one-file HTTP server: honours `Range`, can cut the first response
    /// short, and refuses the first `refuse` requests with 403.
    fn serve(
        body: Vec<u8>,
        cut_first_at: Option<usize>,
        refuse: usize,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        thread::spawn(move || {
            let mut n = 0usize;
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut range = None;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                }
                log.lock().unwrap().push(format!("range={range:?}"));
                n += 1;
                if n <= refuse {
                    let _ = stream.write_all(
                        b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    );
                    continue;
                }
                let start = range.unwrap_or(0);
                let rest = &body[start..];
                let head = if range.is_some() {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\ncontent-length: {}\r\ncontent-range: bytes {start}-{}/{}\r\nconnection: close\r\n\r\n",
                        rest.len(),
                        body.len() - 1,
                        body.len()
                    )
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        rest.len()
                    )
                };
                let _ = stream.write_all(head.as_bytes());
                let send = match cut_first_at {
                    Some(cut) if n == refuse + 1 => &rest[..cut],
                    _ => rest,
                };
                let _ = stream.write_all(send);
            }
        });
        (format!("http://{addr}/file"), seen)
    }

    fn sha(b: &[u8]) -> String {
        format!("{:x}", Sha256::digest(b))
    }

    #[test]
    fn a_cut_download_resumes_with_a_range_and_verifies() {
        let body: Vec<u8> = (0..(3 * CHUNK + 17)).map(|i| (i % 251) as u8).collect();
        let (url, seen) = serve(body.clone(), Some(CHUNK + 5), 0);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b.bin");
        let digest = sha(&body);
        let target = Target {
            path: &path,
            sha256: &digest,
            size: body.len() as u64,
        };
        let mut total = 0u64;
        let got = fetch(&mut || Ok(url.clone()), &target, &mut |n| total += n).unwrap();
        assert_eq!(got, Fetched::Downloaded { resumed_from: 0 });
        assert_eq!(fs::read(&path).unwrap(), body);
        assert_eq!(total, body.len() as u64);
        let log = seen.lock().unwrap().clone();
        assert_eq!(
            log,
            vec![
                "range=None".to_owned(),
                format!("range=Some({})", CHUNK + 5)
            ]
        );
        // Present and verified: no request at all.
        let again = fetch(&mut || panic!("no URL needed"), &target, &mut |_| {}).unwrap();
        assert_eq!(again, Fetched::Present);
    }

    #[test]
    fn an_expired_url_is_renewed_and_a_wrong_digest_is_never_kept() {
        let body = b"exported bytes".to_vec();
        let (url, _) = serve(body.clone(), None, 1);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        let digest = sha(&body);
        let mut renewals = 0;
        let target = Target {
            path: &path,
            sha256: &digest,
            size: body.len() as u64,
        };
        fetch(
            &mut || {
                renewals += 1;
                Ok(url.clone())
            },
            &target,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(renewals, 2);
        let wrong = "0".repeat(64);
        let other = dir.path().join("y.json");
        let bad = Target {
            path: &other,
            sha256: &wrong,
            size: body.len() as u64,
        };
        let e = fetch(&mut || Ok(url.clone()), &bad, &mut |_| {}).unwrap_err();
        assert_eq!(e.code, "artifact_checksum_mismatch");
        assert!(!other.exists() && !part_path(&other).exists());
    }
}
