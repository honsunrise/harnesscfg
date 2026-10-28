//! Filesystem primitives: snapshots for change detection, private copies, and
//! atomic writes that never replace something the user created meanwhile.

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, IoContext, Result};

/// Exists, possibly as a dangling symlink.
pub fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// A symlink into the Nix store: left over from Home Manager owning the file.
pub fn owned(path: &Path, store: &Path) -> bool {
    path.is_symlink() && fs::canonicalize(path).is_ok_and(|p| p.starts_with(store))
}

/// Identity and content of a file or directory tree, to detect edits made
/// between planning and writing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    inode: u64,
    mtime: i128,
    digest: [u8; 32],
}

pub fn snapshot(path: &Path) -> Result<Option<Snapshot>> {
    use std::os::unix::fs::MetadataExt;
    let Ok(link) = fs::symlink_metadata(path) else {
        return Ok(None);
    };
    if !path.exists() {
        return Err(Error::DanglingLink {
            path: path.to_path_buf(),
        });
    }
    let mut digest = Sha256::new();
    let entries = if path.is_dir() {
        walk(path)?
    } else {
        vec![path.to_path_buf()]
    };
    for entry in entries {
        digest.update(entry.as_os_str().as_encoded_bytes());
        let meta = fs::metadata(&entry).at(&entry)?;
        if meta.is_file() {
            digest.update(fs::read(&entry).at(&entry)?);
        } else if !meta.is_dir() {
            return Err(Error::Unsupported { path: entry });
        }
    }
    Ok(Some(Snapshot {
        inode: link.ino(),
        mtime: i128::from(link.mtime()) * 1_000_000_000 + i128::from(link.mtime_nsec()),
        digest: digest.finalize().into(),
    }))
}

/// Every path below `root`, sorted, following file symlinks but not
/// descending through directory symlinks.
fn walk(root: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).at(&dir)? {
            let path = entry.at(&dir)?.path();
            if fs::symlink_metadata(&path).at(&path)?.is_dir() {
                pending.push(path.clone());
            }
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

pub fn mkdir(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .at(path)
}

/// Content to place at a target: a file or tree to copy, or bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Path(PathBuf),
    Bytes(Vec<u8>),
}

/// Write a private copy of `source` (dereferencing symlinks) at `target`.
pub fn copy(source: &Content, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        mkdir(parent)?;
    }
    match source {
        Content::Bytes(bytes) => fs::write(target, bytes).at(target)?,
        Content::Path(path) if path.is_dir() => copy_tree(path, target)?,
        Content::Path(path) => drop(fs::copy(path, target).at(target)?),
    }
    private(target)
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    mkdir(target)?;
    for entry in fs::read_dir(source).at(source)? {
        let entry = entry.at(source)?;
        let (from, to) = (entry.path(), target.join(entry.file_name()));
        let meta = fs::metadata(&from).map_err(|_| Error::DanglingLink { path: from.clone() })?;
        if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else if meta.is_file() {
            fs::copy(&from, &to).at(&to)?;
        } else {
            return Err(Error::Unsupported { path: from });
        }
    }
    Ok(())
}

/// Owner-only permissions; the owner execute bit of a file is kept.
fn private(path: &Path) -> Result<()> {
    let set = |path: &Path| -> Result<()> {
        let meta = fs::symlink_metadata(path).at(path)?;
        let mode = if meta.is_dir() {
            0o700
        } else {
            0o600 | (meta.permissions().mode() & 0o100)
        };
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).at(path)
    };
    set(path)?;
    if path.is_dir() {
        for entry in walk(path)? {
            set(&entry)?;
        }
    }
    Ok(())
}

/// Place `source` (plus `overlays` inside a directory) at `target` atomically.
///
/// `expected` is the snapshot taken while planning; if the target changed
/// since, nothing is written. A new target is created without replacing
/// anything that appeared meanwhile.
pub fn write(
    target: &Path,
    source: &Content,
    expected: Option<&Snapshot>,
    overlays: &[(PathBuf, Vec<u8>)],
    store: &Path,
) -> Result<()> {
    if target.is_symlink() && !owned(target, store) {
        return Err(Error::UserLink {
            path: target.to_path_buf(),
        });
    }
    let parent = target.parent().ok_or_else(|| Error::Unsupported {
        path: target.to_path_buf(),
    })?;
    mkdir(parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".harnesscfg.")
        .tempdir_in(parent)
        .at(parent)?;
    let staged = staging.path().join("value");
    copy(source, &staged)?;
    for (name, bytes) in overlays {
        copy(&Content::Bytes(bytes.clone()), &staged.join(name))?;
    }
    if snapshot(target)?.as_ref() != expected {
        return Err(Error::Concurrent {
            path: target.to_path_buf(),
        });
    }
    if staged.is_dir() {
        if expected.is_some() {
            if !owned(target, store) {
                return Err(Error::UserDirectory {
                    path: target.to_path_buf(),
                });
            }
            fs::remove_file(target).at(target)?;
        }
        rename_noreplace(&staged, target)
    } else if expected.is_none() {
        // Fails if the target appeared since the snapshot.
        fs::hard_link(&staged, target).at(target)
    } else {
        fs::rename(&staged, target).at(target)
    }
}

fn rename_noreplace(from: &Path, to: &Path) -> Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE).at(to)
}

/// Store paths referenced by `content`, such as a statusline command.
pub fn store_paths(content: &Content, store: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut scan = |bytes: &[u8]| scan_store_paths(bytes, store, &mut found);
    match content {
        Content::Bytes(bytes) => scan(bytes),
        Content::Path(path) if path.is_dir() => {
            for entry in walk(path)? {
                if entry.is_file() {
                    scan(&fs::read(&entry).at(&entry)?);
                }
            }
        }
        Content::Path(path) => scan(&fs::read(path).at(path)?),
    }
    found.sort();
    found.dedup();
    Ok(found)
}

/// `<store>/<32 base32 chars>-<name>` up to the next separator.
fn scan_store_paths(bytes: &[u8], store: &Path, found: &mut Vec<PathBuf>) {
    let mut prefix = store.as_os_str().as_encoded_bytes().to_vec();
    prefix.push(b'/');
    let mut rest = bytes;
    while let Some(at) = find(rest, &prefix) {
        let tail = &rest[at + prefix.len()..];
        let hash_ok = tail.len() > 33
            && tail[..32]
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && tail[32] == b'-';
        if hash_ok {
            let end = tail[33..]
                .iter()
                .position(|b| b.is_ascii_whitespace() || b"/\"'<>\\".contains(b))
                .map_or(tail.len(), |n| n + 33);
            if end > 33
                && let Ok(name) = std::str::from_utf8(&tail[..end])
            {
                found.push(store.join(name));
            }
        }
        rest = &rest[at + prefix.len()..];
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_references_stop_at_separators() {
        let store = Path::new("/nix/store");
        let hash = "a".repeat(32);
        let text = format!(
            "cmd: \"/nix/store/{hash}-hud/bin/x\" and /nix/store/{hash}-coreutils-9.11 '/nix/store/short-x'"
        );
        let mut found = Vec::new();
        scan_store_paths(text.as_bytes(), store, &mut found);
        assert_eq!(
            found,
            [
                store.join(format!("{hash}-hud")),
                store.join(format!("{hash}-coreutils-9.11"))
            ]
        );
    }

    #[test]
    fn new_targets_never_replace_files_that_appeared() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.json");
        fs::write(&target, "mine").unwrap();
        let error = write(
            &target,
            &Content::Bytes(b"new".to_vec()),
            None,
            &[],
            Path::new("/nix/store"),
        )
        .unwrap_err();
        assert!(matches!(error, Error::Concurrent { .. }));
        assert_eq!(fs::read_to_string(&target).unwrap(), "mine");
    }

    #[test]
    fn bundles_are_private_and_existing_directories_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("store/bundle");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), "{}").unwrap();
        fs::set_permissions(
            source.join("package.json"),
            fs::Permissions::from_mode(0o444),
        )
        .unwrap();
        let target = dir.path().join("home/bundle");
        write(
            &target,
            &Content::Path(source.clone()),
            None,
            &[],
            Path::new("/nix/store"),
        )
        .unwrap();
        assert_eq!(
            fs::metadata(target.join("package.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(
            write(
                &target,
                &Content::Path(source),
                None,
                &[],
                Path::new("/nix/store")
            )
            .is_err()
        );
    }
}
