# harnesscfg

Seed and sync the writable configuration of AI coding agent harnesses (Claude
Code, Codex, OpenCode, oh-my-pi, Pi, Gemini CLI and others) from Nix, without
taking the files away from the agents.

```text
 Nix template ──┐                             ┌─► ~/.codex/config.toml   (writable, yours)
 (store, r/o)   ├─► harnesscfg apply ─────────┤
 prev template ─┘   at Home Manager switch    └─► only Nix-owned fields change;
                                                  comments, keys and secrets stay
```

## Why

Home Manager normally links configuration files into the read-only Nix store.
Agent harnesses do not cope with that: they save settings, log in, add
projects, trust folders and rewrite their own config files. Linking breaks
them; copying once means Nix can never update model catalogs again.

harnesscfg splits ownership per field instead of per file:

| Who | Owns | When it is written |
| --- | --- | --- |
| Nix | the template, plus the fields a `sync` spec declares (model catalogs, enabled model lists, preset model fields) | every activation |
| The harness and you | everything else: credentials, endpoints, MCP servers, preferences, comments | whenever you like |

It runs once per Home Manager activation and exits. It is not a daemon and
does not watch files.

## What it does

For every managed file, on every activation:

1. **Missing file** → create it from the template.
2. **First time a structured file is managed** → keep every existing value
   and add only the keys the template has and the file lacks.
3. **Leftover read-only store link** → replace it with a writable copy.
4. **`sync` spec** → three-way merge of the Nix-owned fields against the
   previous generation's template: update what Nix declares, drop what Nix
   declared before and no longer does, keep what you added (custom models,
   custom providers).

Everything else in the file is left byte-for-byte as it was. JSON (with
comments), TOML and YAML are edited through lossless syntax trees: comments,
key order, quoting and layout outside the changed nodes survive.

## Guarantees

- **All or nothing.** Every file is planned before any is written. One
  unreadable file aborts the activation and writes nothing.
- **No lost edits.** Each target is snapshotted (inode, mtime, SHA-256) while
  planning and re-checked just before an atomic rename. A file that appears or
  changes meanwhile is never overwritten.
- **No secrets in logs.** Errors name paths, never file contents.
- **Your symlinks are yours.** A symlink you created, or a dangling one, stops
  the activation instead of being followed or replaced.
- **Private by default.** Everything harnesscfg writes is `0600`/`0700`.
- **Originals kept.** The pre-write content of each file is saved once per
  generation under `backupDir`, and Nix store paths a written file references
  (for example a statusline command) get a GC root.
- **Runs alongside oh-my-pi.** OMP's own file lock is taken while its files are
  rewritten, so OMP can keep running and neither side loses a change.
- **No hidden state.** The previous generation's manifest is the only memory.
  There is no database to repair.

## Quick start

```nix
# flake.nix
{
  inputs.harnesscfg.url = "github:honsunrise/harnesscfg";
  inputs.harnesscfg.inputs.nixpkgs.follows = "nixpkgs";
}
```

```nix
# a Home Manager module
{ harnesscfg, pkgs, ... }:
let
  json = pkgs.formats.json { };
  yaml = pkgs.formats.yaml { };
in
{
  imports = [ harnesscfg.homeModules.default ];

  programs.harnesscfg = {
    enable = true;
    files = {
      # Seeded once; afterwards Codex owns the file.
      "codex/config.toml".source = ./codex.toml;

      # Model catalog follows Nix; your API key and custom models stay.
      "omp/models.yml" = {
        source = yaml.generate "models.yml" {
          providers.hub = {
            baseUrl = "https://llm.example/v1";
            models = [ { id = "gpt-5"; contextWindow = 400000; } ];
          };
        };
        sync.kind = "providers";
      };

      # Enabled list follows Nix; ids you enabled yourself stay in front.
      "omp/config.yml" = {
        source = yaml.generate "config.yml" { enabledModels = [ "hub/gpt-5" ]; };
        sync.kind = "enabled";
      };

      # A file outside the registry: give the target explicitly.
      "claude-code/opus.json" = {
        source = json.generate "opus.json" { model = "claude-opus-5-5"; };
        target = "/home/me/.claude/opus.json";
        seed = "copy";
        sync.kind = "preset";
      };
    };
  };
}
```

Attribute names are `<harness id>/<path below its config directory>`. The
harness id selects the default target directory, file format and lock from the
registry, so most entries need only `source`. The name is also the file's
identity across generations: rename it and harnesscfg treats the file as newly
managed.

Inside a terminal:

```console
$ harnesscfg agents            # harnesses on this machine and where their config lives
$ harnesscfg plan  <manifest>  # what apply would write
$ harnesscfg status <manifest> # per-file state: missing / store-link / writable, pending / in sync
```

## Sync kinds

| `sync.kind` | Nix owns | Typical file |
| --- | --- | --- |
| `enabled` | the list `field` (default `enabledModels`); ids you added stay first | `omp/config.yml`, `pi/settings.json` |
| `providers` | `<field>.<provider>.<catalog>` records by id (default `providers.*.{models,modelOverrides}`); a provider Nix adds is copied whole | `omp/models.yml`, `pi/models.json`, `opencode/opencode.json` |
| `preset` | `keys` and model-related `env` entries of a preset file | Claude / Codex model presets |
| `patch` | the provider catalogs inside one entry (`id`) of a YAML patch list `file` in a directory bundle | DeepSeek Harness profiles |

Full schema and defaults: [`docs/MANIFEST.md`](docs/MANIFEST.md).

## Supported harnesses

Paths below are verified against each vendor's documentation or source
(`registry/<id>.toml` lists the sources). Any harness can be managed by giving
an explicit `target`; the registry only supplies defaults.

<!-- harnesses:begin (generated by `harnesscfg agents --format markdown`) -->
| Harness | Config directory | Files |
| --- | --- | --- |
| [aider](https://aider.chat) | `~` | `.aider.conf.yml`, `.aider.model.settings.yml`, `.aider.model.metadata.json` |
| [Amp](https://ampcode.com) | `~/.config` | `amp/settings.json`, `amp/AGENTS.md`, `AGENTS.md` |
| [Auggie](https://docs.augmentcode.com/cli/overview) | `~/.augment` | `settings.json` |
| [Claude Code](https://code.claude.com) | `~/.claude` | `settings.json`, `CLAUDE.md`, `plugins/claude-hud/config.json`, `claude-hud.json` |
| [Claude Desktop](https://claude.ai/download) (macOS only) | `~/Library/Application Support/Claude` | `claude_desktop_config.json` |
| [Cline CLI](https://docs.cline.bot/cli/cli-reference) | `~` | `.cline/data/settings/global-settings.json`, `.cline/data/settings/cline_mcp_settings.json`, `.agents/AGENTS.md` |
| [Codex CLI](https://github.com/openai/codex) | `~/.codex` | `config.toml`, `AGENTS.md`, `AGENTS.override.md` |
| [Continue](https://www.continue.dev) | `~/.continue` | `config.yaml` |
| [Crush](https://github.com/charmbracelet/crush) | `$XDG_CONFIG_HOME/crush` | `crushrc`, `crush.json`, `CRUSH.md` |
| [Cursor CLI](https://cursor.com/docs/cli/overview) | `~/.cursor` | `cli-config.json`, `mcp.json` |
| [Devin Desktop](https://docs.devin.ai/desktop/cascade/mcp) | `$XDG_CONFIG_HOME/devin` | `mcp_config.json` |
| [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) | `~/.dsh` | `cordis.patch.yml`, `profiles/web/package.json`, `profiles/web/cordis.patch.yml`, `profiles/sdk/package.json`, `profiles/sdk/cordis.patch.yml`, `profiles/headless/package.json`, `profiles/headless/cordis.patch.yml`, `profiles/acp/package.json`, `profiles/acp/cordis.patch.yml` |
| [Droid](https://docs.factory.com/droid-cli/settings) | `~/.factory` | `settings.json`, `settings.local.json`, `mcp.json`, `AGENTS.md` |
| [Gemini CLI](https://geminicli.com) | `~/.gemini` | `settings.json`, `GEMINI.md` |
| [GitHub Copilot CLI](https://github.com/github/copilot-cli) | `~/.copilot` | `settings.json`, `mcp-config.json`, `lsp-config.json`, `providers.json`, `copilot-instructions.md` |
| [goose](https://github.com/block/goose) | `$XDG_CONFIG_HOME/goose` | `config.yaml`, `permission.yaml` |
| [Herdr](https://github.com/herdrdev/herdr) | `$XDG_CONFIG_HOME/herdr` | `config.toml` |
| [iFlow CLI](https://github.com/iflow-ai/iflow-cli) | `~/.iflow` | `settings.json`, `IFLOW.md` |
| [Kilo Code CLI](https://kilo.ai) | `$XDG_CONFIG_HOME/kilo` | `kilo.jsonc`, `kilo.json`, `config.json`, `opencode.json`, `opencode.jsonc`, `tui.json`, `tui.jsonc`, `AGENTS.md` |
| [Kimi Code CLI](https://moonshotai.github.io/kimi-cli/) | `~/.kimi` | `config.toml`, `mcp.json` |
| [oh-my-pi](https://github.com/can1357/oh-my-pi) | `~/.omp/agent` | `config.yml`, `models.yml`, `mcp.json` |
| [OpenCode](https://opencode.ai) | `$XDG_CONFIG_HOME/opencode` | `opencode.json`, `cli.json`, `tui.json` |
| [OpenHands CLI](https://github.com/OpenHands/OpenHands-CLI) | `~/.openhands` | `agent_settings.json`, `cli_config.json`, `mcp.json` |
| [Pi](https://github.com/earendil-works/pi) | `~/.pi/agent` | `settings.json`, `models.json`, `SYSTEM.md`, `APPEND_SYSTEM.md`, `extensions/subagent/config.json`, `AGENTS.md`, `AGENTS.override.md`, `CLAUDE.md` |
| [Qwen Code](https://github.com/QwenLM/qwen-code) | `~/.qwen` | `settings.json`, `QWEN.md` |
| [GitHub Copilot in Visual Studio Code](https://code.visualstudio.com/docs/copilot/overview) | `~/.config/Code/User` (macOS: `~/Library/Application Support/Code/User`) | `settings.json`, `mcp.json` |
| [Windsurf](https://windsurf.com) | `~/.codeium/windsurf` | `mcp_config.json`, `memories/global_rules.md` |
| [Zed](https://zed.dev) | `$XDG_CONFIG_HOME/zed` (macOS: `~/.config/zed`) | `settings.json` |
<!-- harnesses:end -->

Adding a harness is a data change: see
[`registry/README.md`](registry/README.md).

## For AI agents working on this repository

Read [`AGENTS.md`](AGENTS.md) first. It lists the invariants, the module map
and the commands to verify a change. [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
explains the data flow.

## License

MIT
