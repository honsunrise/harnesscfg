//! Activation: lock, plan, back up, protect store references, write.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};
use crate::fsops::{self, Content, exists, owned};
use crate::lock;
use crate::manifest::{self, Lock, Manifest};
use crate::plan::{self, Write};

pub struct Options {
    /// Plan and report, write nothing, take no locks.
    pub dry_run: bool,
    /// The Nix store; tests point this at a fixture directory.
    pub store: PathBuf,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            dry_run: false,
            store: PathBuf::from("/nix/store"),
        }
    }
}

pub struct Report {
    /// `(entry name, target)` of every planned write.
    pub writes: Vec<(String, PathBuf)>,
    /// Where originals were saved; `None` if nothing was written.
    pub backup: Option<PathBuf>,
}

pub fn run(
    manifest_path: &Path,
    previous_path: Option<&Path>,
    options: &Options,
) -> Result<Report> {
    let manifest = Manifest::load(manifest_path)?;
    let previous = manifest::previous(previous_path)?;
    // OMP re-reads and merges under this lock, so it can keep running. A
    // read-only store link cannot be saved by OMP and has no writable sidecar.
    let mut locked: Vec<&Path> = manifest
        .entries
        .iter()
        .filter(|e| e.lock == Lock::Omp && !options.dry_run && !owned(&e.target, &options.store))
        .map(|e| e.target.as_path())
        .collect();
    locked.sort();
    locked.dedup();
    let _guards = locked
        .into_iter()
        .map(lock::acquire)
        .collect::<Result<Vec<_>>>()?;

    let writes = plan::plan(&manifest.entries, &previous, &options.store)?;
    let report = |backup| Report {
        writes: writes
            .iter()
            .map(|w| (w.name.clone(), w.target.clone()))
            .collect(),
        backup,
    };
    if options.dry_run || writes.is_empty() {
        return Ok(report(None));
    }
    let name = manifest_path
        .file_name()
        .map_or_else(|| "manifest".into(), |n| n.to_string_lossy().into_owned());
    let backup = manifest.backup_dir.join(format!("nix.{name}"));
    protect(
        &writes,
        &backup,
        manifest.nix_store.as_deref(),
        &options.store,
    )?;
    for write in &writes {
        fsops::write(
            &write.target,
            &write.content,
            write.expected.as_ref(),
            &write.overlays,
            &options.store,
        )?;
    }
    Ok(report(Some(backup)))
}

/// Save originals once per generation and keep referenced store paths alive:
/// a written config outlives the generation that produced its references.
fn protect(writes: &[Write], backup: &Path, nix_store: Option<&Path>, store: &Path) -> Result<()> {
    fsops::mkdir(backup)?;
    let mut roots = Vec::new();
    for write in writes {
        let original = backup.join("user").join(&write.name);
        if write.target.exists() {
            let current = Content::Path(write.target.clone());
            roots.extend(fsops::store_paths(&current, store)?);
            if !exists(&original) {
                fsops::copy(&current, &original)?;
            }
        }
        roots.extend(fsops::store_paths(&write.content, store)?);
        for (_, bytes) in &write.overlays {
            roots.extend(fsops::store_paths(&Content::Bytes(bytes.clone()), store)?);
        }
    }
    roots.sort();
    roots.dedup();
    let Some(nix_store) = nix_store else {
        return Ok(());
    };
    for root in roots {
        let link = backup
            .join("store-roots")
            .join(root.file_name().expect("store paths have a name"));
        if root.extension().is_some_and(|e| e == "drv") || !root.exists() || exists(&link) {
            continue;
        }
        fsops::mkdir(link.parent().expect("has a parent"))?;
        let status = Command::new(nix_store)
            .arg("--realise")
            .arg(&root)
            .arg("--add-root")
            .arg(&link)
            .arg("--indirect")
            .stdout(std::process::Stdio::null())
            .status()
            .map_err(|source| Error::Io {
                path: nix_store.to_path_buf(),
                source,
            })?;
        if !status.success() {
            return Err(Error::Command {
                command: format!("{} --realise", nix_store.display()),
                status: status.to_string(),
            });
        }
    }
    Ok(())
}
