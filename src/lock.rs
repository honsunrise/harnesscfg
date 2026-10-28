//! oh-my-pi's cooperative file lock.
//!
//! OMP saves `config.yml`/`models.yml` under `pi-natives` `FileLock` and
//! re-reads the file inside the lock, so holding the same lock while
//! rewriting lets OMP keep running without losing either side's changes.
//!
//! * Linux: an abstract Unix datagram socket named
//!   `omp-file-lock-<xxh64(path, seed1)><xxh64(path, seed2)>`.
//! * macOS: `flock(LOCK_EX)` on a persistent `<path>.lock` sidecar.
//!
//! `<path>` is OMP's write path: the target's `realpath` when it exists, else
//! the target as given (OMP's `path.resolve`, which does not follow symlinks).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Error, IoContext, Result};

const ATTEMPTS: u32 = 50;
const DELAY: Duration = Duration::from_millis(100);

/// Held until dropped.
pub struct Guard {
    _handle: Handle,
}

#[cfg(target_os = "linux")]
type Handle = std::os::unix::net::UnixDatagram;
#[cfg(not(target_os = "linux"))]
type Handle = std::fs::File;

pub fn acquire(target: &Path) -> Result<Guard> {
    if let Some(parent) = target.parent() {
        crate::fsops::mkdir(parent)?;
    }
    let mut lock_path = resolve(target).into_os_string();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);
    for attempt in 1..=ATTEMPTS {
        match try_lock(&lock_path)? {
            Some(handle) => return Ok(Guard { _handle: handle }),
            None if attempt < ATTEMPTS => std::thread::sleep(DELAY),
            None => {}
        }
    }
    Err(Error::Locked {
        path: target.to_path_buf(),
    })
}

#[cfg(target_os = "linux")]
fn try_lock(lock_path: &Path) -> Result<Option<Handle>> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    let bytes = lock_path.as_os_str().as_encoded_bytes();
    let high = xxhash_rust::xxh64::xxh64(bytes, 0x4F4D_502D_4C4F_434B);
    let low = xxhash_rust::xxh64::xxh64(bytes, 0x5049_2D46_494C_454C);
    let name = format!("omp-file-lock-{high:016x}{low:016x}");
    let address = SocketAddr::from_abstract_name(name.as_bytes()).at(lock_path)?;
    match UnixDatagram::bind_addr(&address) {
        Ok(socket) => Ok(Some(socket)),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => Ok(None),
        Err(error) => Err(Error::Io {
            path: lock_path.to_path_buf(),
            source: error,
        }),
    }
}

#[cfg(not(target_os = "linux"))]
fn try_lock(lock_path: &Path) -> Result<Option<Handle>> {
    use rustix::fs::{FlockOperation, flock};
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(lock_path)
        .at(lock_path)?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(file)),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
        Err(errno) => Err(Error::Io {
            path: lock_path.to_path_buf(),
            source: errno.into(),
        }),
    }
}

/// OMP's `#resolveYamlWritePath`: `realpath` of an existing file, else the
/// absolute path unchanged. Dangling links never reach here: planning refuses them.
fn resolve(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_blocks_a_second_holder_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("agent/config.yml");
        let guard = acquire(&target).unwrap();
        let lock_path = PathBuf::from(format!("{}.lock", resolve(&target).display()));
        assert!(try_lock(&lock_path).unwrap().is_none());
        drop(guard);
        assert!(try_lock(&lock_path).unwrap().is_some());
    }

    #[test]
    fn missing_targets_lock_their_literal_path_like_omp() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(resolve(&link.join("config.yml")), link.join("config.yml"));
        std::fs::write(real.join("config.yml"), "").unwrap();
        assert_eq!(
            resolve(&link.join("config.yml")),
            std::fs::canonicalize(real.join("config.yml")).unwrap()
        );
    }
}
