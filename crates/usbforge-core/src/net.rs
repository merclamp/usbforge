//! ISO downloading (Rufus's Fido / download features).
//!
//! A streaming HTTP downloader with progress and SHA-256 verification (pure-Rust
//! HTTP + TLS via `ureq`/rustls, so no system OpenSSL and it works on Windows
//! too), plus a small "resolve the latest ISO URL" helper for distros that
//! publish a stable index. Generic `https://` URLs cover everything else.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::report::{Reporter, ReporterExt};
use crate::{Error, Result};

/// Download `url` to `dest`, streaming with progress. If `expected_sha256` is
/// given, the download is rejected on a hash mismatch. Returns bytes written.
pub fn download_to_file(
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
    reporter: &dyn Reporter,
) -> Result<u64> {
    reporter.info(&format!("Downloading {url}"));
    let resp = ureq::get(url)
        .call()
        .map_err(|e| Error::Other(format!("HTTP request failed: {e}")))?;

    let total: u64 = resp
        .header("Content-Length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut reader = resp.into_reader();
    let mut file = File::create(dest)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        file.write_all(chunk)?;
        hasher.update(chunk);
        done += n as u64;
        if total > 0 {
            reporter.progress("download", done as f32 / total as f32);
        }
    }
    reporter.progress("download", 1.0);
    file.flush()?;

    if let Some(expected) = expected_sha256 {
        let got = hex::encode(hasher.finalize());
        if !got.eq_ignore_ascii_case(expected.trim()) {
            return Err(Error::Other(format!(
                "SHA-256 mismatch: expected {expected}, got {got}"
            )));
        }
        reporter.info("SHA-256 verified.");
    }
    Ok(done)
}

/// Fetch a small text resource (used to resolve distro release indexes).
pub fn fetch_text(url: &str) -> Result<String> {
    ureq::get(url)
        .call()
        .map_err(|e| Error::Other(format!("HTTP request failed: {e}")))?
        .into_string()
        .map_err(|e| Error::Other(format!("reading response: {e}")))
}

/// Resolve the latest Alpine ISO of a given flavour (e.g. `virt`, `standard`,
/// `extended`) for x86_64, returning `(url, Option<sha256>)`. Reads Alpine's
/// published `latest-releases.yaml`.
pub fn resolve_alpine(flavor: &str) -> Result<(String, Option<String>)> {
    const BASE: &str = "https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/x86_64";
    let yaml = fetch_text(&format!("{BASE}/latest-releases.yaml"))?;
    let (file, sha) = parse_alpine_release(&yaml, flavor).ok_or_else(|| {
        Error::Other(format!(
            "no Alpine '{flavor}' x86_64 ISO found in the release index"
        ))
    })?;
    Ok((format!("{BASE}/{file}"), sha))
}

/// Find `(filename, sha256?)` for the given Alpine flavour in a
/// `latest-releases.yaml` body. Pure, so it's unit-testable without the network.
fn parse_alpine_release(yaml: &str, flavor: &str) -> Option<(String, Option<String>)> {
    let prefix = format!("alpine-{flavor}-");
    let lines: Vec<&str> = yaml.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if let Some(rest) = line.trim().strip_prefix("file:") {
            let file = rest.trim();
            if file.starts_with(&prefix) && file.ends_with(".iso") {
                // The sha256 field appears within the same release block.
                let sha = lines[i + 1..(i + 8).min(lines.len())]
                    .iter()
                    .find_map(|l| l.trim().strip_prefix("sha256:"))
                    .map(|s| s.trim().to_string());
                return Some((file.to_string(), sha));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpine_yaml_parsing() {
        let yaml = "\
- title: Virtual
  file: alpine-virt-3.99.0-x86_64.iso
  flavor: alpine-virt
  sha256: abc123def456
- title: Standard
  file: alpine-standard-3.99.0-x86_64.iso
  sha256: deadbeef
";
        let (file, sha) = parse_alpine_release(yaml, "virt").unwrap();
        assert_eq!(file, "alpine-virt-3.99.0-x86_64.iso");
        assert_eq!(sha.as_deref(), Some("abc123def456"));

        let (file2, _) = parse_alpine_release(yaml, "standard").unwrap();
        assert_eq!(file2, "alpine-standard-3.99.0-x86_64.iso");
        assert!(parse_alpine_release(yaml, "nonexistent").is_none());
    }
}
