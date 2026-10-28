//! The manifest: what Home Manager asks harnesscfg to do.
//!
//! The Home Manager module writes one manifest per generation into the
//! generation itself (immutable, garbage-collected with it). Activation reads
//! the current manifest and the previous generation's; the previous one is the
//! only memory harnesscfg has, so there is no state file to drift or repair.
//! See `docs/MANIFEST.md` for the schema.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, IoContext, Result};
use crate::format::Format;

pub const VERSION: u32 = 2;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub entries: Vec<Entry>,
    /// Where originals of overwritten files and GC roots are kept.
    pub backup_dir: PathBuf,
    /// `nix-store` used to register GC roots for store paths a written file
    /// references. Without it, references are not protected from GC.
    #[serde(default)]
    pub nix_store: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Entry {
    /// Stable identity across generations, e.g. `codex/config.toml`.
    pub name: String,
    /// Registry id, e.g. `codex`. Informational; policy lives in the entry.
    pub harness: String,
    /// Template in the Nix store: a file, or a directory bundle.
    pub source: PathBuf,
    /// Writable location the client reads.
    pub target: PathBuf,
    /// How to parse the file. `null` for opaque files and directory bundles.
    #[serde(default)]
    pub format: Option<Format>,
    /// `merge`: the first time the file is managed, add missing template keys
    /// to an existing user file. `copy`: only create the file when missing.
    #[serde(default)]
    pub seed: Seed,
    /// Nix-owned fields kept in sync on every activation.
    #[serde(default)]
    pub sync: Option<Sync>,
    /// Sibling names the client accepts for the same file, e.g.
    /// `opencode.jsonc` for `opencode.json`. An existing one is used instead.
    #[serde(default)]
    pub alternates: Vec<PathBuf>,
    #[serde(default)]
    pub lock: Lock,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Seed {
    #[default]
    Merge,
    Copy,
}

/// Cooperative write lock the client itself honours while saving.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lock {
    #[default]
    None,
    /// oh-my-pi's `pi-natives` FileLock.
    Omp,
}

/// Which fields of a file Nix owns.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Sync {
    /// A list of enabled model ids; custom ids stay in front.
    Enabled {
        #[serde(default = "enabled_field")]
        field: String,
    },
    /// Top-level model fields and model-related `env` entries of a preset.
    #[serde(rename_all = "camelCase")]
    Preset {
        #[serde(default = "preset_keys")]
        keys: Vec<String>,
        #[serde(default = "preset_env_keys")]
        env_keys: Vec<String>,
        #[serde(default = "preset_env_prefixes")]
        env_prefixes: Vec<String>,
    },
    /// Model catalogs under `<field>.<provider>.<catalog>`.
    Providers {
        #[serde(default = "providers_field")]
        field: String,
        #[serde(default = "catalogs")]
        catalogs: Vec<String>,
    },
    /// A YAML patch list inside a directory bundle; catalogs live under
    /// `[id=<id>].config.<field>.<provider>.<catalog>`.
    Patch {
        id: String,
        file: PathBuf,
        #[serde(default = "providers_field")]
        field: String,
        #[serde(default = "catalogs")]
        catalogs: Vec<String>,
    },
}

impl Sync {
    pub fn preset_default() -> Sync {
        Sync::Preset {
            keys: preset_keys(),
            env_keys: preset_env_keys(),
            env_prefixes: preset_env_prefixes(),
        }
    }
}

fn enabled_field() -> String {
    "enabledModels".into()
}
fn providers_field() -> String {
    "providers".into()
}
fn catalogs() -> Vec<String> {
    vec!["models".into(), "modelOverrides".into()]
}
fn preset_keys() -> Vec<String> {
    [
        "model",
        "review_model",
        "model_context_window",
        "model_reasoning_effort",
    ]
    .map(String::from)
    .to_vec()
}
fn preset_env_keys() -> Vec<String> {
    [
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
        "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
        "CLAUDE_CODE_EFFORT_LEVEL",
    ]
    .map(String::from)
    .to_vec()
}
fn preset_env_prefixes() -> Vec<String> {
    vec!["ANTHROPIC_CUSTOM_MODEL_OPTION".into()]
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Manifest> {
        let raw = std::fs::read(path).at(path)?;
        let invalid = |reason: String| Error::Manifest {
            path: path.to_path_buf(),
            reason,
        };
        let manifest: Manifest =
            serde_json::from_slice(&raw).map_err(|e| invalid(e.to_string()))?;
        if manifest.version != VERSION {
            return Err(invalid(format!(
                "version {} is not {VERSION}",
                manifest.version
            )));
        }
        let mut names = BTreeSet::new();
        for entry in &manifest.entries {
            if !names.insert(entry.name.as_str()) {
                return Err(invalid(format!("duplicate entry {}", entry.name)));
            }
            if !entry.target.is_absolute() || !entry.source.is_absolute() {
                return Err(invalid(format!(
                    "{}: source and target must be absolute",
                    entry.name
                )));
            }
            let needs_format = entry.seed == Seed::Merge
                || matches!(
                    entry.sync,
                    Some(Sync::Enabled { .. } | Sync::Preset { .. } | Sync::Providers { .. })
                );
            if needs_format && entry.format.is_none() {
                return Err(invalid(format!(
                    "{}: merge and model sync need a format",
                    entry.name
                )));
            }
        }
        Ok(manifest)
    }
}

/// What an earlier generation recorded about an entry. Only identity and
/// template location matter, so any manifest version is readable.
#[derive(Clone, Debug, Deserialize)]
pub struct Previous {
    pub name: String,
    pub target: PathBuf,
    pub source: PathBuf,
}

/// Entries of the previous generation's manifest, or none when it is absent
/// (first activation, or a generation that did not manage any file).
pub fn previous(path: Option<&Path>) -> Result<Vec<Previous>> {
    let Some(path) = path.filter(|p| p.is_file()) else {
        return Ok(Vec::new());
    };
    #[derive(Deserialize)]
    struct Loose {
        entries: Vec<Previous>,
    }
    let raw = std::fs::read(path).at(path)?;
    let loose: Loose = serde_json::from_slice(&raw).map_err(|e| Error::Manifest {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    Ok(loose.entries)
}
