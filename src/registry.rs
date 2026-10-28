//! The harness registry: verified facts about where each harness keeps its
//! user-level configuration. Data only; see `registry/README.md`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/registry.rs"));
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Harness {
    pub id: String,
    pub name: String,
    pub homepage: String,
    pub kind: Kind,
    pub binaries: Vec<String>,
    pub config_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir_macos: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir_env: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<Platform>,
    pub lock: crate::manifest::Lock,
    pub verified: String,
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(rename = "file")]
    pub files: Vec<File>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Cli,
    Desktop,
    Editor,
    Multiplexer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Linux,
    Macos,
}

impl Platform {
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::Macos
        } else {
            Platform::Linux
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub name: String,
    pub format: FileFormat,
    pub role: Role,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternate_extensions: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileFormat {
    Json,
    Toml,
    Yaml,
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Settings,
    Models,
    Mcp,
    Rules,
    Other,
}

/// Every embedded harness, validated, sorted by id.
pub fn all() -> Result<Vec<Harness>> {
    embedded::FILES
        .iter()
        .map(|(stem, text)| parse(stem, text))
        .collect()
}

pub fn parse(stem: &str, text: &str) -> Result<Harness> {
    let invalid = |reason: String| Error::Registry {
        id: stem.to_owned(),
        reason,
    };
    let harness: Harness = toml::from_str(text).map_err(|e| invalid(e.message().to_owned()))?;
    harness.validate(stem).map_err(invalid)?;
    Ok(harness)
}

impl Harness {
    fn validate(&self, stem: &str) -> std::result::Result<(), String> {
        let id_ok = self.id.starts_with(|c: char| c.is_ascii_lowercase())
            && self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !id_ok || self.id != stem {
            return Err(format!(
                "id {:?} must be [a-z][a-z0-9-]* and equal the file stem",
                self.id
            ));
        }
        for dir in std::iter::once(&self.config_dir).chain(&self.config_dir_macos) {
            if expand(dir, Path::new("/h"), |_| None).is_none() {
                return Err(format!("config_dir {dir:?} uses an unsupported form"));
            }
        }
        let date = self.verified.as_bytes();
        let date_ok = date.len() == 10
            && date[4] == b'-'
            && date[7] == b'-'
            && self
                .verified
                .replace('-', "")
                .bytes()
                .all(|b| b.is_ascii_digit());
        if !date_ok {
            return Err("verified must be YYYY-MM-DD".into());
        }
        if self.sources.is_empty() || !self.sources.iter().all(|s| s.starts_with("https://")) {
            return Err("sources must be non-empty https URLs".into());
        }
        if !self.homepage.starts_with("https://") {
            return Err("homepage must be an https URL".into());
        }
        if self.files.is_empty() {
            return Err("at least one [[file]] is required".into());
        }
        let mut names = std::collections::BTreeSet::new();
        for file in &self.files {
            let relative = |p: &str| {
                !p.is_empty()
                    && !p.starts_with('/')
                    && !p.split('/').any(|c| c.is_empty() || c == "." || c == "..")
            };
            if !relative(&file.name) || file.target.as_deref().is_some_and(|t| !relative(t)) {
                return Err(format!(
                    "file {:?} must be a clean relative path",
                    file.name
                ));
            }
            if !names.insert(&file.name) {
                return Err(format!("duplicate file {:?}", file.name));
            }
        }
        Ok(())
    }

    pub fn supports(&self, platform: Platform) -> bool {
        self.platforms.is_empty() || self.platforms.contains(&platform)
    }

    /// The configuration root on this machine: `config_dir_env` when set,
    /// else the platform's documented default.
    pub fn config_root(
        &self,
        home: &Path,
        env: impl Fn(&str) -> Option<String>,
    ) -> Option<PathBuf> {
        if let Some(dir) = self
            .config_dir_env
            .as_deref()
            .and_then(&env)
            .filter(|d| !d.is_empty())
        {
            return Some(PathBuf::from(dir));
        }
        let dir = match (Platform::current(), &self.config_dir_macos) {
            (Platform::Macos, Some(dir)) => dir,
            _ => &self.config_dir,
        };
        expand(dir, home, env)
    }
}

/// Expand `~`, `~/…` and `$XDG_{CONFIG,DATA,STATE}_HOME/…`.
pub fn expand(dir: &str, home: &Path, env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if dir == "~" {
        return Some(home.to_path_buf());
    }
    if let Some(rest) = dir.strip_prefix("~/") {
        return Some(home.join(rest));
    }
    for (var, fallback) in [
        ("XDG_CONFIG_HOME", ".config"),
        ("XDG_DATA_HOME", ".local/share"),
        ("XDG_STATE_HOME", ".local/state"),
    ] {
        if let Some(rest) = dir.strip_prefix('$').and_then(|d| d.strip_prefix(var)) {
            if !(rest.is_empty() || rest.starts_with('/')) {
                return None;
            }
            let base = env(var)
                .filter(|v| v.starts_with('/'))
                .map_or_else(|| home.join(fallback), PathBuf::from);
            return Some(base.join(rest.trim_start_matches('/')));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_entry_is_valid() {
        let harnesses = all().unwrap();
        assert!(harnesses.len() >= 20);
        assert!(
            harnesses
                .iter()
                .any(|h| h.id == "omp" && h.lock == crate::manifest::Lock::Omp)
        );
    }

    #[test]
    fn invalid_entries_are_rejected() {
        let base = r#"id = "x"
name = "X"
homepage = "https://x"
kind = "cli"
binaries = []
config_dir = "~/.x"
lock = "none"
verified = "2026-10-28"
sources = ["https://x"]
[[file]]
name = "a.json"
format = "json"
role = "settings"
description = "d"
"#;
        assert!(parse("x", base).is_ok());
        assert!(parse("y", base).is_err(), "id must equal stem");
        assert!(parse("x", &base.replace("~/.x", "/abs")).is_err());
        assert!(parse("x", &base.replace("a.json", "../a.json")).is_err());
        assert!(parse("x", &base.replace("https://x\"]", "http://x\"]")).is_err());
        assert!(parse("x", &format!("{base}extra = 1\n")).is_err());
    }

    #[test]
    fn config_dirs_expand_xdg_with_fallbacks() {
        let home = Path::new("/home/u");
        assert_eq!(
            expand("$XDG_CONFIG_HOME/zed", home, |_| None).unwrap(),
            Path::new("/home/u/.config/zed")
        );
        assert_eq!(
            expand("$XDG_CONFIG_HOME/zed", home, |_| Some("/x".into())).unwrap(),
            Path::new("/x/zed")
        );
        assert_eq!(expand("~", home, |_| None).unwrap(), home);
        assert!(expand("$HOME/x", home, |_| None).is_none());
        assert!(expand("$XDG_CONFIG_HOMEx", home, |_| None).is_none());
    }
}
