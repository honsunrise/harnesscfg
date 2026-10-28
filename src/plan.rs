//! Planning: decide, for every entry, what (if anything) to write.
//!
//! Every entry is planned before anything is written, so one unreadable or
//! inconsistent file aborts the activation without touching the others.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{Error, Result};
use crate::format::{Edit, Format, Root};
use crate::fsops::{Content, Snapshot, exists, owned, snapshot};
use crate::manifest::{Entry, Previous, Seed, Sync};
use crate::merge;

/// One planned write.
#[derive(Debug)]
pub struct Write {
    /// Entry name, plus the patch file for writable bundles.
    pub name: String,
    pub target: PathBuf,
    pub content: Content,
    /// Snapshot of `target` while planning; `None` if it did not exist.
    pub expected: Option<Snapshot>,
    /// Files replaced inside a copied directory bundle.
    pub overlays: Vec<(PathBuf, Vec<u8>)>,
}

pub fn plan(entries: &[Entry], previous: &[Previous], store: &Path) -> Result<Vec<Write>> {
    let mut writes = Vec::new();
    for entry in entries {
        let old = previous
            .iter()
            .find(|p| p.name == entry.name && p.target == entry.target);
        if let Some(write) = plan_entry(entry, old, store)? {
            writes.push(write);
        }
    }
    Ok(writes)
}

fn plan_entry(entry: &Entry, old: Option<&Previous>, store: &Path) -> Result<Option<Write>> {
    if let Some(Sync::Patch { file, .. }) = &entry.sync {
        return plan_bundle(entry, old, file, store);
    }
    let target = resolve_alternates(entry)?;
    let previous = old.map(|o| o.source.clone());
    let file = File {
        target: &target,
        template: &entry.source,
        previous: previous.as_deref(),
        format: entry.format,
        root: Root::Object,
    };
    plan_file(entry, old, &entry.name, file, store)
}

/// A managed file: where it lives, which template backs it now and backed it
/// in the previous generation, and how to read it.
struct File<'a> {
    target: &'a Path,
    template: &'a Path,
    previous: Option<&'a Path>,
    format: Option<Format>,
    root: Root,
}

fn plan_file(
    entry: &Entry,
    old: Option<&Previous>,
    name: &str,
    file: File,
    store: &Path,
) -> Result<Option<Write>> {
    let expected = snapshot(file.target)?;
    let link = owned(file.target, store);
    if old.is_some() && !link && entry.sync.is_none() {
        return Ok(None); // Initialized by an earlier generation; the client owns it now.
    }
    let mut doc: Option<Doc> = None;
    if old.is_none() && entry.seed == Seed::Merge {
        let format = file
            .format
            .expect("manifest validation requires a format to merge");
        let mut d = Doc::open(&file, format, expected.is_some(), true)?;
        let template = read(file.template, format, file.root)?;
        if let (Value::Object(defaults), Value::Object(_)) = (template.value(), d.edit.value()) {
            let mut filled = d.edit.value().clone();
            if merge::fill_missing(filled.as_object_mut().expect("checked above"), defaults) {
                d.changed |= d.edit.update(filled, file.target)?;
            }
        }
        doc = Some(d);
    }
    if let Some(spec) = &entry.sync {
        let format = file
            .format
            .expect("manifest validation requires a format to sync");
        let mirror = old.is_none() && entry.seed == Seed::Copy;
        let mut d = match doc.take() {
            Some(d) => d,
            None => Doc::open(&file, format, expected.is_some(), mirror)?,
        };
        sync(&mut d, spec, &file, format)?;
        doc = Some(d);
    }
    let content = match finish(doc, expected.is_some())? {
        Outcome::Keep => {
            if !link {
                return Ok(None);
            }
            Content::Path(file.target.to_path_buf()) // Turn a store link into a writable copy.
        }
        Outcome::Template => Content::Path(file.template.to_path_buf()),
        Outcome::Bytes(bytes) => Content::Bytes(bytes),
    };
    if file.target.is_symlink() && !link {
        return Err(Error::UserLink {
            path: file.target.to_path_buf(),
        });
    }
    Ok(Some(Write {
        name: name.to_owned(),
        target: file.target.to_path_buf(),
        content,
        expected,
        overlays: Vec::new(),
    }))
}

/// A directory bundle whose patch file holds the model catalog.
fn plan_bundle(
    entry: &Entry,
    old: Option<&Previous>,
    file: &Path,
    store: &Path,
) -> Result<Option<Write>> {
    let target = &entry.target;
    if exists(target) && !target.is_dir() {
        return Err(Error::NotDirectory {
            path: target.clone(),
        });
    }
    let spec = entry.sync.as_ref().expect("bundles always sync");
    let template = entry.source.join(file);
    let previous = old.map(|o| o.source.join(file));
    let patch = target.join(file);
    let patch_file = File {
        target: &patch,
        template: &template,
        previous: previous.as_deref(),
        format: Some(Format::Yaml),
        root: Root::Array,
    };
    if owned(target, store) || !exists(target) {
        // Copy the whole bundle; only the patch may differ from the template.
        let mirror = old.is_none() && entry.seed == Seed::Copy && !exists(target);
        let mut doc = Doc::open(&patch_file, Format::Yaml, exists(&patch), mirror)?;
        sync(&mut doc, spec, &patch_file, Format::Yaml)?;
        let overlays = match finish(Some(doc), exists(&patch))? {
            Outcome::Bytes(bytes) => vec![(file.to_path_buf(), bytes)],
            Outcome::Keep | Outcome::Template => Vec::new(),
        };
        let source = if exists(target) {
            target.clone()
        } else {
            entry.source.clone()
        };
        return Ok(Some(Write {
            name: entry.name.clone(),
            target: target.clone(),
            content: Content::Path(source),
            expected: snapshot(target)?,
            overlays,
        }));
    }
    if target.is_symlink() {
        return Err(Error::UserLink {
            path: target.clone(),
        });
    }
    // A writable bundle belongs to the client except its patch file.
    let name = format!("{}/{}", entry.name, file.display());
    plan_file(entry, old, &name, patch_file, store)
}

fn sync(doc: &mut Doc, spec: &Sync, file: &File, format: Format) -> Result<()> {
    let current = read(file.template, format, file.root)?;
    let previous = match file.previous {
        Some(path) => read(path, format, file.root)?.value().clone(),
        None => empty(file.root),
    };
    let mut user = doc.edit.value().clone();
    merge::sync(spec, &mut user, current.value(), &previous).map_err(|reason| Error::Sync {
        path: file.target.to_path_buf(),
        reason,
    })?;
    doc.changed |= doc.edit.update(user, file.target)?;
    Ok(())
}

/// The working copy of one file.
struct Doc {
    edit: Edit,
    changed: bool,
    /// The edit started from the template's text, not from the target.
    mirrors_template: bool,
}

impl Doc {
    /// The target's content; if it does not exist, the template's text when
    /// `mirror` is set, an empty document otherwise.
    fn open(file: &File, format: Format, target_exists: bool, mirror: bool) -> Result<Doc> {
        let (edit, mirrors_template) = if target_exists {
            (read(file.target, format, file.root)?, false)
        } else if mirror {
            (read(file.template, format, file.root)?, true)
        } else {
            (Edit::empty(format, file.root), false)
        };
        Ok(Doc {
            edit,
            changed: false,
            mirrors_template,
        })
    }
}

enum Outcome {
    /// The target already holds the right content.
    Keep,
    /// The target is missing and the template's bytes are what it needs.
    Template,
    Bytes(Vec<u8>),
}

fn finish(doc: Option<Doc>, target_exists: bool) -> Result<Outcome> {
    Ok(match doc {
        Some(doc) if doc.changed => Outcome::Bytes(doc.edit.render()?),
        Some(doc) if !target_exists && !doc.mirrors_template => Outcome::Bytes(doc.edit.render()?),
        _ if target_exists => Outcome::Keep,
        _ => Outcome::Template,
    })
}

fn read(path: &Path, format: Format, root: Root) -> Result<Edit> {
    let raw = std::fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Edit::parse(format, root, &raw, path)
}

fn empty(root: Root) -> Value {
    match root {
        Root::Object => Value::Object(Default::default()),
        Root::Array => Value::Array(Vec::new()),
    }
}

/// The client may keep the file under a sibling name (`opencode.jsonc`).
pub fn resolve_alternates(entry: &Entry) -> Result<PathBuf> {
    let parent = entry.target.parent().unwrap_or(Path::new("/"));
    let mut present: Vec<PathBuf> = std::iter::once(entry.target.clone())
        .chain(entry.alternates.iter().map(|alt| parent.join(alt)))
        .filter(|path| exists(path))
        .collect();
    match present.len() {
        0 => Ok(entry.target.clone()),
        1 => Ok(present.remove(0)),
        _ => Err(Error::Ambiguous { paths: present }),
    }
}
