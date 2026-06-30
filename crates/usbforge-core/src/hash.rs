//! Checksums for image verification (Rufus `hash.c`).
//!
//! Pure-Rust implementations via the RustCrypto crates — no system crypto
//! library, identical on Linux and Windows. All requested algorithms are
//! computed in a single streaming pass over the file, with progress reported
//! through a [`Reporter`].

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

use crate::report::Reporter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Algo {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl Algo {
    pub fn name(self) -> &'static str {
        match self {
            Algo::Md5 => "MD5",
            Algo::Sha1 => "SHA-1",
            Algo::Sha256 => "SHA-256",
            Algo::Sha512 => "SHA-512",
        }
    }

    /// All algorithms, in display order.
    pub fn all() -> [Algo; 4] {
        [Algo::Md5, Algo::Sha1, Algo::Sha256, Algo::Sha512]
    }
}

enum Hasher {
    Md5(Md5),
    Sha1(Sha1),
    Sha256(Sha256),
    Sha512(Sha512),
}

impl Hasher {
    fn for_algo(algo: Algo) -> Hasher {
        match algo {
            Algo::Md5 => Hasher::Md5(Md5::new()),
            Algo::Sha1 => Hasher::Sha1(Sha1::new()),
            Algo::Sha256 => Hasher::Sha256(Sha256::new()),
            Algo::Sha512 => Hasher::Sha512(Sha512::new()),
        }
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::Md5(h) => h.update(data),
            Hasher::Sha1(h) => h.update(data),
            Hasher::Sha256(h) => h.update(data),
            Hasher::Sha512(h) => h.update(data),
        }
    }

    fn finalize_hex(self) -> String {
        match self {
            Hasher::Md5(h) => hex::encode(h.finalize()),
            Hasher::Sha1(h) => hex::encode(h.finalize()),
            Hasher::Sha256(h) => hex::encode(h.finalize()),
            Hasher::Sha512(h) => hex::encode(h.finalize()),
        }
    }
}

/// Compute the requested checksums of `path` in one pass.
///
/// Returns a map from algorithm to lowercase hex digest.
pub fn hash_file(
    path: impl AsRef<Path>,
    algos: &[Algo],
    reporter: &dyn Reporter,
) -> crate::Result<BTreeMap<Algo, String>> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let total = file.metadata()?.len();
    let mut reader = std::io::BufReader::with_capacity(1 << 20, file);

    let mut hashers: Vec<(Algo, Hasher)> = algos
        .iter()
        .copied()
        .map(|a| (a, Hasher::for_algo(a)))
        .collect();

    let mut buf = vec![0u8; 1 << 20];
    let mut done: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        for (_, h) in hashers.iter_mut() {
            h.update(chunk);
        }
        done += n as u64;
        if total > 0 {
            reporter.progress("hash", done as f32 / total as f32);
        }
    }
    reporter.progress("hash", 1.0);

    let mut out = BTreeMap::new();
    for (algo, hasher) in hashers {
        out.insert(algo, hasher.finalize_hex());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::NullReporter;
    use std::io::Write;

    #[test]
    fn known_vectors_for_abc() {
        // "abc" digests are standard test vectors.
        let mut tmp = std::env::temp_dir();
        tmp.push("usbforge_hash_abc.bin");
        {
            let mut f = File::create(&tmp).unwrap();
            f.write_all(b"abc").unwrap();
        }
        let out = hash_file(&tmp, &Algo::all(), &NullReporter).unwrap();
        assert_eq!(out[&Algo::Md5], "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(out[&Algo::Sha1], "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            out[&Algo::Sha256],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(&tmp);
    }
}
