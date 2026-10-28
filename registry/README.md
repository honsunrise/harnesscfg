# Harness registry

One file per harness (`<id>.toml`). The registry records **facts about where a
harness keeps its configuration**. It never records policy: which fields Nix
owns, model catalogs and merge strategy live in the consumer's module options,
not here.

The registry is compiled into the binary (`harnesscfg agents`) and read by the
Home Manager module to default each file's `format` and location.

## Schema

```toml
id          = "aider"                  # [a-z][a-z0-9-]*, equal to the file stem
name        = "Aider"                  # product name as the vendor writes it
homepage    = "https://aider.chat"
kind        = "cli"                    # cli | desktop | editor | multiplexer
binaries    = ["aider"]                # executables used for detection; [] if none
config_dir  = "~"                      # root of every `file.name` below
# Optional:
config_dir_macos = "~/Library/Application Support/Foo"  # when macOS differs
config_dir_env   = "CODEX_HOME"        # env var the tool honours to relocate config_dir
platforms        = ["macos"]          # macos | linux; omit when the facts hold on every platform
lock        = "none"                   # none | omp  (omp: tool saves under OMP's FileLock)
verified    = "2026-10-28"             # date the facts below were checked
sources     = ["https://..."]          # official docs / source files proving the facts
notes       = "short caveats"          # optional

[[file]]
name        = ".aider.conf.yml"        # path relative to config_dir (may contain "/")
format      = "yaml"                   # json | toml | yaml | text
role        = "settings"               # settings | models | mcp | rules | other
description = "Main configuration file."
# Optional:
target      = "relative/path"          # when the file actually lives elsewhere than `name`
alternate_extensions = ["jsonc"]       # extensions the tool also accepts for the same file
```

## Allowed `config_dir` forms

`~`, `~/path`, `$XDG_CONFIG_HOME/path`, `$XDG_DATA_HOME/path`,
`$XDG_STATE_HOME/path`. XDG variables fall back to `~/.config`, `~/.local/share`
and `~/.local/state`. Nothing else is expanded.

## Rules

- **Only verified facts.** Every `file` must be backed by a URL in `sources`
  (official docs, or the tool's own source file). If you cannot verify a path or
  format, leave the file out and say so in the research notes. Do not guess.
- **Only user-level files.** Skip project-level files (`.cursor/rules` inside a
  repo, `AGENTS.md` in a workspace) and anything that holds credentials or
  session state (`auth.json`, `.credentials.json`, history, caches).
- **Only files a person edits or a tool reads at startup.** Prefer `settings`,
  `models`, `mcp`, `rules`. Use `other` sparingly.
- `format = "json"` covers JSON with comments / trailing commas. Markdown and
  other prose files are `text`.
- One harness per file; keep `description` to one sentence; no secrets.
- English only.
