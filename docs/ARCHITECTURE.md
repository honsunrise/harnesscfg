# Architecture

```text
manifest.json ─┐
prev manifest ─┴► apply::run ─► lock::acquire (OMP targets)
                                 │
                                 ▼
                            plan::plan ── for each entry ──► plan_file / plan_bundle
                                 │                              │
                                 │                 format::Edit (lossless doc + value)
                                 │                              │ merge::fill_missing
                                 │                              │ merge::sync (three-way)
                                 │                              │ diff::diff → Ops → backend
                                 ▼                              ▼
                            Vec<Write> ◄───────────────── Write{target, content, snapshot}
                                 │
                       backups + GC roots (apply::protect)
                                 │
                            fsops::write (snapshot re-check, atomic rename)
```

## Modules

| Module | Responsibility |
| --- | --- |
| `cli` | Argument parsing, output, `umask 077`. |
| `apply` | Orchestration: locks, plan, backups, GC roots, writes. |
| `plan` | Per-entry decision table (see `docs/MANIFEST.md`). Pure apart from reads. |
| `merge` | Format-neutral rules on `serde_json::Value`: `fill_missing`, `sync`. |
| `diff` | Turns a before/after value pair into replayable `Op`s (keyed by `id` for record lists). |
| `format` | `Edit`: one parsed file. Backends `json` (jsonc-parser CST), `toml` (toml_edit), `yaml` (yaml-edit) replay `Op`s on their syntax trees. |
| `fsops` | Snapshots, private copies, atomic no-replace writes, store reference scanning. |
| `lock` | oh-my-pi FileLock compatibility (abstract socket on Linux, `flock` sidecar on macOS). |
| `manifest` | Schema, validation, reading previous manifests of any version. |
| `registry` | Embedded `registry/*.toml`, validation, path expansion. |

## Why diff and replay

Merge rules are easiest to get right on plain values, but rewriting a file
from a value loses comments and formatting the user cares about. So merges
run on values, and the result is applied as the minimal edit to the original
syntax tree. `format` tests check every backend against the reference replay
in `diff::apply` by re-parsing the output.

## Why the previous manifest

Three-way merging needs to know what Nix declared last time, to tell "Nix
retired this model" from "the user added this model". The previous
generation's manifest points at its templates, which the old generation keeps
alive in the store. Nothing else is persisted.

## Failure model

Planning reads everything and fails on the first problem; no write has
happened. Writes then happen one file at a time; each is atomic and refuses to
replace a file that changed since planning. A crash between two writes leaves
some files updated and others untouched, both consistent; the next activation
finishes the rest.
