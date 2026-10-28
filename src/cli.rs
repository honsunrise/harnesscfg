//! Command-line interface.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};

use crate::apply::{self, Options};
use crate::error::Result;
use crate::fsops::{exists, owned};
use crate::manifest::{self, Manifest};
use crate::registry::{self, Platform};

/// Seed and sync the writable configuration of AI coding agent harnesses
/// from Nix templates.
#[derive(Parser)]
#[command(name = "harnesscfg", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Apply a manifest: seed missing files and sync Nix-owned fields.
    /// Writes nothing when the DRY_RUN environment variable is set.
    Apply(Run),
    /// Show what `apply` would write, without writing or locking.
    Plan(Run),
    /// Per-file state of a manifest: missing, store link, writable, pending.
    Status(Run),
    /// List the harnesses harnesscfg knows and where they keep configuration.
    Agents {
        #[arg(long, value_enum, default_value_t = Output::Table)]
        format: Output,
    },
    /// Validate a manifest and the embedded registry.
    Check { manifest: Option<PathBuf> },
}

#[derive(clap::Args)]
struct Run {
    /// Manifest of the generation being activated.
    manifest: PathBuf,
    /// Manifest of the previous generation; empty or missing on first use.
    #[arg(long)]
    previous: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Output {
    Table,
    Json,
    Markdown,
}

pub fn run() -> i32 {
    // Everything harnesscfg creates is private: configs hold credentials.
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    match dispatch(Cli::parse().command) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("harnesscfg: {error}");
            1
        }
    }
}

fn dispatch(command: Command) -> Result<()> {
    match command {
        Command::Apply(run) => {
            let options = Options {
                dry_run: std::env::var_os("DRY_RUN").is_some(),
                ..Options::default()
            };
            report(&run, &options)
        }
        Command::Plan(run) => report(
            &run,
            &Options {
                dry_run: true,
                ..Options::default()
            },
        ),
        Command::Status(run) => status(&run),
        Command::Agents { format } => agents(format),
        Command::Check { manifest } => {
            let harnesses = registry::all()?;
            println!("registry: {} harnesses valid", harnesses.len());
            if let Some(path) = manifest {
                let manifest = Manifest::load(&path)?;
                println!("manifest: {} entries valid", manifest.entries.len());
            }
            Ok(())
        }
    }
}

fn previous(run: &Run) -> Option<&Path> {
    run.previous
        .as_deref()
        .filter(|p| !p.as_os_str().is_empty())
}

fn report(run: &Run, options: &Options) -> Result<()> {
    let report = apply::run(&run.manifest, previous(run), options)?;
    let verb = if options.dry_run {
        "would write"
    } else {
        "wrote"
    };
    for (name, target) in &report.writes {
        println!("{verb} {name} -> {}", target.display());
    }
    if let Some(backup) = report.backup {
        println!("originals saved in {}", backup.display());
    }
    Ok(())
}

fn status(run: &Run) -> Result<()> {
    let options = Options {
        dry_run: true,
        ..Options::default()
    };
    let manifest = Manifest::load(&run.manifest)?;
    let previous = manifest::previous(previous(run))?;
    let writes = crate::plan::plan(&manifest.entries, &previous, &options.store)?;
    let width = manifest
        .entries
        .iter()
        .map(|e| e.name.len())
        .max()
        .unwrap_or(0);
    for entry in &manifest.entries {
        let target = crate::plan::resolve_alternates(entry)?;
        let state = if !exists(&target) {
            "missing"
        } else if owned(&target, &options.store) {
            "store-link"
        } else if target.is_symlink() {
            "user-link"
        } else {
            "writable"
        };
        let pending = writes
            .iter()
            .any(|w| w.name == entry.name || w.name.starts_with(&format!("{}/", entry.name)));
        let action = if pending { "pending" } else { "in sync" };
        println!(
            "{:width$}  {state:10}  {action:7}  {}",
            entry.name,
            target.display()
        );
    }
    Ok(())
}

fn agents(format: Output) -> Result<()> {
    let harnesses = registry::all()?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let env = |name: &str| std::env::var(name).ok();
    let platform = Platform::current();
    match format {
        Output::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&harnesses).expect("registry serializes")
            );
        }
        Output::Markdown => print!("{}", markdown(&harnesses)),
        Output::Table => {
            for h in harnesses.iter().filter(|h| h.supports(platform)) {
                let root = h.config_root(&home, env);
                let present = root.as_deref().is_some_and(Path::exists);
                let installed = h.binaries.iter().any(|b| on_path(b));
                let root = root.map_or_else(|| "?".into(), |p| p.display().to_string());
                let marks = format!(
                    "{}{}",
                    if installed { "installed" } else { "" },
                    if present {
                        if installed { ", config" } else { "config" }
                    } else {
                        ""
                    }
                );
                println!("{:22} {:28} {:44} {marks}", h.id, h.name, root);
            }
        }
    }
    Ok(())
}

/// The README's harness table, generated so the two never disagree.
pub fn markdown(harnesses: &[registry::Harness]) -> String {
    let mut out = String::from("| Harness | Config directory | Files |\n| --- | --- | --- |\n");
    for h in harnesses {
        let dir = match &h.config_dir_macos {
            Some(macos) => format!("`{}` (macOS: `{macos}`)", h.config_dir),
            None => format!("`{}`", h.config_dir),
        };
        let only = if h.platforms.is_empty() {
            String::new()
        } else {
            format!(
                " ({} only)",
                h.platforms
                    .iter()
                    .map(|p| match p {
                        Platform::Linux => "Linux",
                        Platform::Macos => "macOS",
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let files = h
            .files
            .iter()
            .map(|f| format!("`{}`", f.target.as_deref().unwrap_or(&f.name)))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "| [{}]({}){only} | {dir} | {files} |\n",
            h.name, h.homepage
        ));
    }
    out
}

fn on_path(binary: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(binary).is_file()))
}
