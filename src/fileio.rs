//! File helpers: slide hashing (off the UI thread, ANN-14) and atomic writes
//! (ANN-17).

use eov_plugin_api::i18n::{self, tr};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const CHUNK_COUNT: u64 = 256;
const CHUNK_SIZE: usize = 1024;

/// The pre-1.0 slide key: SHA-256 over 256 evenly spaced 1 KiB chunks of the
/// file (the whole file when smaller). Kept to find annotations stored by
/// older versions; identical to `eov_common::file_id::compute_fingerprint`.
pub fn legacy_fingerprint(path: &Path) -> io::Result<[u8; 32]> {
    let len = std::fs::metadata(path)?.len();
    let mut hasher = Sha256::new();
    let mut file = File::open(path)?;
    if len < CHUNK_COUNT * CHUNK_SIZE as u64 {
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;
        hasher.update(&buffer);
        return Ok(hasher.finalize().into());
    }
    let mut chunk = [0u8; CHUNK_SIZE];
    let max_offset = len - CHUNK_SIZE as u64;
    for index in 0..CHUNK_COUNT {
        let offset = if index == 0 {
            0
        } else {
            ((u128::from(index) * u128::from(max_offset)) / u128::from(CHUNK_COUNT - 1)) as u64
        };
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut chunk)?;
        hasher.update(chunk);
    }
    Ok(hasher.finalize().into())
}

/// SHA-256 of a whole file, reporting progress and stopping when `cancel`
/// is set (used only to check exports of pre-1.0 versions).
pub fn sha256_with_progress(
    path: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32),
) -> io::Result<Option<[u8; 32]>> {
    let len = std::fs::metadata(path)?.len().max(1);
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 4 << 20];
    let mut done: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        done += read as u64;
        progress((done as f64 / len as f64) as f32);
    }
    Ok(Some(hasher.finalize().into()))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes `bytes` to `path` atomically: a temporary file in the same
/// directory is written, flushed to disk and renamed over the target.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "export".to_string());
    let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Largest import read into memory.
pub const MAX_IMPORT_BYTES: u64 = 1 << 30;

/// Reads an import file with a size limit; the error is for the user.
pub fn read_limited(path: &Path) -> Result<Vec<u8>, String> {
    let file = || i18n::isolate(path.display());
    let len = std::fs::metadata(path)
        .map_err(|e| tr!("cannot read {file}: {error}", file = file(), error = e))?
        .len();
    if len > MAX_IMPORT_BYTES {
        return Err(tr!(
            "{file} is {size} MB; imports are limited to {limit} MB",
            file = file(),
            size = len >> 20,
            limit = MAX_IMPORT_BYTES >> 20
        ));
    }
    std::fs::read(path).map_err(|e| tr!("cannot read {file}: {error}", file = file(), error = e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same algorithm, written independently, as the host's legacy key.
    fn reference(path: &Path) -> [u8; 32] {
        let data = std::fs::read(path).unwrap();
        let len = data.len() as u64;
        let mut hasher = Sha256::new();
        if len < 256 * 1024 {
            hasher.update(&data);
        } else {
            let max = len - 1024;
            for i in 0..256u64 {
                let offset = if i == 0 {
                    0
                } else {
                    (i as u128 * max as u128 / 255) as u64
                } as usize;
                hasher.update(&data[offset..offset + 1024]);
            }
        }
        hasher.finalize().into()
    }

    #[test]
    fn fingerprints_match_the_legacy_algorithm() {
        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("small.bin");
        std::fs::write(&small, vec![9u8; 1000]).unwrap();
        assert_eq!(legacy_fingerprint(&small).unwrap(), reference(&small));
        let large = dir.path().join("large.bin");
        let bytes: Vec<u8> = (0..(256 * 1024 * 3 + 777))
            .map(|i| (i * 31 % 251) as u8)
            .collect();
        std::fs::write(&large, bytes).unwrap();
        assert_eq!(legacy_fingerprint(&large).unwrap(), reference(&large));
    }

    #[test]
    fn hashing_can_be_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.bin");
        std::fs::write(&file, b"abc").unwrap();
        let cancel = AtomicBool::new(false);
        let mut seen = 0.0;
        let digest = sha256_with_progress(&file, &cancel, |p| seen = p)
            .unwrap()
            .unwrap();
        assert_eq!(
            hex(&digest),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(seen, 1.0);
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(sha256_with_progress(&file, &cancel, |_| {}).unwrap(), None);
    }

    #[test]
    fn atomic_writes_replace_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.json");
        write_atomically(&path, b"one").unwrap();
        write_atomically(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1);
        assert!(write_atomically(&dir.path().join("missing/dir/out.json"), b"x").is_err());
    }
}
