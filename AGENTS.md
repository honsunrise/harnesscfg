# AGENTS.md

Guidance for AI agents (and humans) changing this repository.

## What this is

A Rust CLI plus a Home Manager module. At activation it seeds writable config
files of AI coding harnesses from Nix templates and keeps Nix-owned fields in
sync. Read `README.md`, then `docs/ARCHITECTURE.md`.

## Invariants — never break these

1. Plan every entry before writing anything; any error means zero writes.
2. Never overwrite a file that changed after planning (snapshot re-check).
3. Never print file contents in errors; configs contain API keys.
4. Never follow or replace a symlink the user owns.
5. Never touch a field outside the entry's `sync` spec after first seeding.
6. Preserve comments and formatting outside changed nodes (all three formats).
7. The previous generation's manifest is the only state. Do not add a state file.
8. Registry entries hold verified facts with sources; no guessed paths.

## Layout

- `src/` — library and CLI (module map in `docs/ARCHITECTURE.md`).
- `tests/apply.rs` — end-to-end behaviour on temp directories.
- `tests/readme.rs` — README harness table equals the registry.
- `registry/` — one TOML per harness; schema in `registry/README.md`.
- `nix/` — package and Home Manager module; `flake.nix` exposes both.
- `docs/MANIFEST.md` — manifest schema; update it with any schema change.

## Verify a change

```console
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
nix flake check            # package build (runs tests in the sandbox) + formatting
```

After editing `registry/`, regenerate the README table:

```console
cargo run -q -- agents --format markdown
```

and paste it between the `harnesses:begin`/`harnesses:end` markers.

## Conventions

- English only, in code, docs and messages.
- Behaviour tests go in `tests/apply.rs`; format edge cases in
  `src/format/*` unit tests.
- New manifest fields need serde defaults so older consumers keep working.
