# Manifest

The Home Manager module writes one manifest per generation (JSON, into the
generation itself). `harnesscfg apply <manifest> --previous <old manifest>`
reads it. You only write a manifest by hand when using harnesscfg without the
module.

```json
{
  "version": 2,
  "backupDir": "/home/me/.local/state/harnesscfg",
  "nixStore": "/nix/store/…-nix/bin/nix-store",
  "entries": [
    {
      "name": "omp/models.yml",
      "harness": "omp",
      "source": "/nix/store/…-models.yml",
      "target": "/home/me/.omp/agent/models.yml",
      "format": "yaml",
      "seed": "merge",
      "sync": { "kind": "providers" },
      "alternates": ["models.yaml"],
      "lock": "omp"
    }
  ]
}
```

## Top level

| Field | Required | Meaning |
| --- | --- | --- |
| `version` | yes | `2`. |
| `entries` | yes | Managed files. Names must be unique. |
| `backupDir` | yes | Originals of overwritten files go to `<backupDir>/nix.<manifest file name>/user/<entry name>`, GC roots to `…/store-roots/`. |
| `nixStore` | no | `nix-store` binary used to add GC roots for store paths a written file references. Without it no roots are added. |

## Entry

| Field | Default | Meaning |
| --- | --- | --- |
| `name` | — | Identity across generations, conventionally `<harness>/<path>`. An entry is "already managed" when the previous manifest has the same `name` **and** `target`. |
| `harness` | — | Registry id. Informational; all behaviour comes from the fields below. |
| `source` | — | Absolute path of the template: a file, or a directory bundle. |
| `target` | — | Absolute path the harness reads. |
| `format` | `null` | `json` (comments and trailing commas accepted), `toml`, `yaml`, or `null` for opaque files and bundles. |
| `seed` | `merge` | `merge`: the first time the entry is managed, add missing template keys to an existing file. `copy`: only create the file when missing. `merge` needs a `format`. |
| `sync` | `null` | Nix-owned fields, see below. |
| `alternates` | `[]` | Sibling file names the harness accepts for the same file (`opencode.jsonc`). If exactly one candidate exists it is used; more than one is an error. |
| `lock` | `none` | `omp`: hold oh-my-pi's FileLock while rewriting. |

## What happens per entry

| Situation | Action |
| --- | --- |
| target missing, not managed before | create from template (then apply `sync`) |
| target missing, managed before, no `sync` | nothing — you deleted it on purpose |
| target missing, managed before, `sync` | recreate with only the Nix-owned fields |
| target exists, not managed before, `seed = merge` | add missing template keys |
| target is a symlink into the Nix store | replace with a writable copy |
| target exists, managed before, no `sync` | nothing — the harness owns it |
| `sync` present | three-way merge of Nix-owned fields; write only if they changed |
| target is your own or a dangling symlink | error, nothing written |

## Sync kinds

`current` is this generation's template, `previous` the previous generation's
(empty when the entry is new).

### `enabled`

| Field | Default |
| --- | --- |
| `field` | `enabledModels` |

Result: ids of the user list that neither template mentions, in user order,
followed by `current`'s list. Other keys are untouched.

### `providers`

| Field | Default |
| --- | --- |
| `field` | `providers` |
| `catalogs` | `["models", "modelOverrides"]` |

For each provider in `previous` ∪ `current` that has one of `catalogs`:

- provider missing in the file and present in `current` → copied whole
  (connection settings included);
- otherwise each catalog is merged record by record (lists of objects with a
  unique string `id`, or objects keyed by id): ids `previous` had and
  `current` dropped are removed, ids in `current` get their fields set (fields
  `previous` set and `current` dropped are removed), other records and other
  fields stay. A catalog that ends up empty and that `current` no longer
  declares is removed.

Provider connection settings of existing providers (`baseUrl`, `apiKey`, …)
are never changed.

### `preset`

| Field | Default |
| --- | --- |
| `keys` | `model`, `review_model`, `model_context_window`, `model_reasoning_effort` |
| `envKeys` | `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL`, `CLAUDE_CODE_MAX_CONTEXT_TOKENS`, `CLAUDE_CODE_MAX_OUTPUT_TOKENS`, `CLAUDE_CODE_EFFORT_LEVEL` |
| `envPrefixes` | `ANTHROPIC_CUSTOM_MODEL_OPTION` |

Only those top-level keys and those `env` entries follow the template.

### `patch`

| Field | Default |
| --- | --- |
| `id` | — |
| `file` | — (path inside the bundle) |
| `field` | `providers` |
| `catalogs` | `["models", "modelOverrides"]` |

The entry is a directory bundle whose `file` is a YAML list of
`{id, config}` patches. The patch with `id` gets `providers`-style catalog
merging on `config.<field>`. If the bundle does not exist (or is a store link)
the whole bundle is copied; afterwards only `file` is touched.
