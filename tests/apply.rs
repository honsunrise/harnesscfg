//! Behaviour of `apply` end to end, on throwaway directories.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use harnesscfg::apply::{self, Options};
use serde_json::{Value, json};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: PathBuf,
    entries: Vec<Value>,
    generation: usize,
    previous: Option<PathBuf>,
    nix_store: Option<PathBuf>,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let store = root.join("nix/store");
        fs::create_dir_all(&store).unwrap();
        Fixture {
            _dir: dir,
            root,
            store,
            entries: Vec::new(),
            generation: 0,
            previous: None,
            nix_store: None,
        }
    }

    fn put(&self, path: &Path, text: &str) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
        path.to_path_buf()
    }

    fn home(&self, name: &str) -> PathBuf {
        self.root.join("home").join(name)
    }

    /// Template for `name` in the current generation.
    fn template(&self, name: &str, text: &str) -> PathBuf {
        self.put(
            &self.root.join(format!("gen{}", self.generation)).join(name),
            text,
        )
    }

    fn entry(
        &mut self,
        name: &str,
        harness: &str,
        format: Option<&str>,
        text: &str,
        extra: Value,
    ) -> PathBuf {
        let source = self.template(name, text);
        let target = self.home(name);
        let mut entry = json!({"name": name, "harness": harness, "source": source, "target": target, "format": format});
        entry
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        self.entries.push(entry);
        target
    }

    /// Point entry `name` at a new template, as the next generation would.
    fn retemplate(&mut self, name: &str, text: &str) {
        let source = self.template(name, text);
        let entry = self.entries.iter_mut().find(|e| e["name"] == name).unwrap();
        entry["source"] = json!(source);
    }

    fn manifest(&self) -> PathBuf {
        let manifest = json!({
            "version": 2,
            "entries": self.entries,
            "backupDir": self.root.join("state/harnesscfg"),
            "nixStore": self.nix_store,
        });
        self.put(
            &self
                .root
                .join(format!("gen{}/manifest.json", self.generation)),
            &manifest.to_string(),
        )
    }

    fn apply_with(&self, dry_run: bool) -> harnesscfg::error::Result<apply::Report> {
        let options = Options {
            dry_run,
            store: self.store.clone(),
        };
        apply::run(&self.manifest(), self.previous.as_deref(), &options)
    }

    fn apply(&self) -> apply::Report {
        self.apply_with(false).unwrap()
    }

    /// Finish this generation: its manifest becomes the previous one.
    fn next_generation(&mut self) {
        self.previous = Some(self.manifest());
        self.generation += 1;
    }

    fn backups(&self) -> PathBuf {
        self.root.join("state/harnesscfg")
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn json_of(path: &Path) -> Value {
    serde_json::from_str(&read(path)).unwrap()
}

#[test]
fn dry_run_writes_no_files_and_no_backups() {
    let mut f = Fixture::new();
    let target = f.entry(
        "opencode/config.json",
        "opencode",
        Some("json"),
        r#"{"model":"new"}"#,
        json!({}),
    );
    let report = f.apply_with(true).unwrap();
    assert_eq!(report.writes.len(), 1);
    assert!(!target.exists());
    assert!(!f.backups().exists());
}

#[test]
fn first_merge_keeps_user_values_and_adds_missing_defaults() {
    let mut f = Fixture::new();
    let target = f.entry(
        "opencode/config.json",
        "opencode",
        Some("json"),
        r#"{"model":"new","providers":{"hub":{}},"plugins":["default"]}"#,
        json!({}),
    );
    f.put(
        &target,
        r#"{"model":"mine","plugins":[],"providers":{"custom":{}}}"#,
    );
    f.apply();
    let value = json_of(&target);
    assert_eq!(value["model"], "mine");
    assert_eq!(value["plugins"], json!([]));
    assert_eq!(
        value["providers"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["custom", "hub"]
    );
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn later_generations_neither_resync_defaults_nor_restore_deleted_files() {
    let mut f = Fixture::new();
    let target = f.entry(
        "omp/config.yml",
        "omp",
        Some("yaml"),
        "theme: dark\n",
        json!({}),
    );
    f.apply();
    f.next_generation();
    f.retemplate("omp/config.yml", "theme: light\nnewSetting: true\n");
    fs::remove_file(&target).unwrap();
    assert!(f.apply().writes.is_empty());
    assert!(!target.exists());
}

#[test]
fn a_newly_enabled_harness_initializes_after_an_existing_generation() {
    let mut f = Fixture::new();
    let omp = f.entry(
        "omp/config.yml",
        "omp",
        Some("yaml"),
        "theme: dark\n",
        json!({}),
    );
    f.apply();
    f.next_generation();
    let pi = f.entry(
        "pi/settings.json",
        "pi",
        Some("json"),
        r#"{"defaultProvider":"openai"}"#,
        json!({}),
    );
    f.apply();
    assert!(omp.exists());
    assert_eq!(json_of(&pi)["defaultProvider"], "openai");
}

#[test]
fn a_moved_target_initializes_the_new_location() {
    let mut f = Fixture::new();
    let old = f.entry(
        "opencode/config.json",
        "opencode",
        Some("json"),
        r#"{"model":"default"}"#,
        json!({}),
    );
    f.apply();
    f.next_generation();
    let new = f.root.join("new-xdg/opencode/config.json");
    f.entries[0]["target"] = json!(new);
    f.apply();
    assert_eq!(json_of(&new)["model"], "default");
    assert_eq!(json_of(&old)["model"], "default");
}

#[test]
fn leftover_store_links_become_writable_copies() {
    let mut f = Fixture::new();
    let target = f.entry(
        "omp/models.yml",
        "omp",
        None,
        "providers:\n  old: {}\n",
        json!({"seed": "copy"}),
    );
    let linked = f.put(
        &f.store.join(format!("{}-models.yml", "a".repeat(32))),
        "providers:\n  old: {}\n",
    );
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&linked, &target).unwrap();
    f.apply();
    assert!(!target.is_symlink());
    assert_eq!(read(&target), read(&linked));
}

#[test]
fn toml_merge_keeps_comments_trust_and_custom_servers() {
    let mut f = Fixture::new();
    let target = f.entry(
        "codex/config.toml",
        "codex",
        Some("toml"),
        "model = \"default\"\n[mcp_servers.new]\ncommand = \"new\"\n",
        json!({}),
    );
    f.put(&target, "# mine\nmodel = \"mine\"\n[projects.\"/work\"]\ntrust_level = \"trusted\"\n[mcp_servers.custom]\ncommand = \"mine\"\n");
    f.apply();
    let text = read(&target);
    let value: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(value["model"].as_str(), Some("mine"));
    assert_eq!(
        value["projects"]["/work"]["trust_level"].as_str(),
        Some("trusted")
    );
    assert!(value["mcp_servers"]["custom"].is_table() && value["mcp_servers"]["new"].is_table());
    assert!(text.starts_with("# mine\n"), "{text}");
}

#[test]
fn yaml_merge_keeps_comments_and_yaml_12_strings() {
    let mut f = Fixture::new();
    let target = f.entry(
        "omp/config.yml",
        "omp",
        Some("yaml"),
        "newSetting: true\n",
        json!({}),
    );
    f.put(
        &target,
        "# mine\nthinking: off\nquoted: \"on\"\nnumber: 0123\n",
    );
    f.apply();
    let text = read(&target);
    for kept in [
        "# mine",
        "thinking: off",
        "quoted: \"on\"",
        "number: 0123",
        "newSetting: true",
    ] {
        assert!(text.contains(kept), "{kept} missing:\n{text}");
    }
}

#[test]
fn jsonc_comments_survive_a_merge() {
    let mut f = Fixture::new();
    let target = f.entry(
        "opencode/opencode.json",
        "opencode",
        Some("json"),
        r#"{"model":"x","share":"disabled"}"#,
        json!({}),
    );
    f.put(
        &target,
        "{\n  // why I chose this\n  \"model\": \"mine\",\n}\n",
    );
    f.apply();
    let text = read(&target);
    assert!(text.contains("// why I chose this"), "{text}");
    assert!(text.contains("\"share\""), "{text}");
}

#[test]
fn an_alternate_extension_is_used_when_present() {
    let mut f = Fixture::new();
    let target = f.entry(
        "opencode/opencode.json",
        "opencode",
        Some("json"),
        r#"{"a":1}"#,
        json!({"alternates": ["opencode.jsonc"]}),
    );
    let jsonc = target.with_extension("jsonc");
    f.put(&jsonc, "{}");
    f.apply();
    assert!(!target.exists());
    assert_eq!(json_of(&jsonc), json!({"a": 1}));
    f.put(&target, "{}");
    assert!(
        f.apply_with(false).is_err(),
        "both names present must be refused"
    );
}

#[test]
fn an_invalid_file_stops_every_write_without_quoting_it() {
    let mut f = Fixture::new();
    let first = f.entry("pi/settings.json", "pi", Some("json"), "{}", json!({}));
    let invalid = f.entry(
        "opencode/config.json",
        "opencode",
        Some("json"),
        "{}",
        json!({}),
    );
    f.put(&invalid, "invalid JSON secret-value");
    let error = f.apply_with(false).err().unwrap();
    assert!(!error.to_string().contains("secret-value"));
    assert!(!first.exists());
}

#[test]
fn dangling_and_user_owned_links_are_left_alone() {
    let mut f = Fixture::new();
    let target = f.entry(
        "opencode/config.json",
        "opencode",
        Some("json"),
        r#"{"new":true}"#,
        json!({}),
    );
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(f.root.join("absent"), &target).unwrap();
    assert!(f.apply_with(false).is_err());
    fs::remove_file(&target).unwrap();
    let mine = f.put(&f.root.join("user-config.json"), "{}");
    std::os::unix::fs::symlink(&mine, &target).unwrap();
    assert!(f.apply_with(false).is_err());
    assert_eq!(read(&mine), "{}");
}

#[test]
fn a_busy_omp_lock_leaves_everything_untouched_and_a_retry_succeeds() {
    let mut f = Fixture::new();
    let target = f.entry(
        "omp/config.yml",
        "omp",
        Some("yaml"),
        "enabledModels: [hub/new]\n",
        json!({"sync": {"kind": "enabled"}, "lock": "omp"}),
    );
    let original = "enabledModels: [hub/old]\nmodelRoles: {default: hub/old}\n";
    f.put(&target, original);
    let guard = harnesscfg::lock::acquire(&target).unwrap();
    assert!(matches!(
        f.apply_with(false),
        Err(harnesscfg::error::Error::Locked { .. })
    ));
    assert_eq!(read(&target), original);
    assert!(!f.backups().exists());
    drop(guard);
    f.apply();
    let text = read(&target);
    assert!(text.contains("default: hub/old"), "{text}");
    assert!(
        text.contains("hub/old") && text.contains("hub/new"),
        "{text}"
    );
}

#[test]
fn model_catalog_upgrades_keep_custom_models_and_credentials() {
    let mut f = Fixture::new();
    let sync = json!({"sync": {"kind": "providers"}});
    let target = f.entry("pi/models.json", "pi", Some("json"), r#"{"providers":{"hub":{"apiKey":"template","models":[{"id":"keep","cw":100,"obsolete":true},{"id":"retire"}]}}}"#, sync);
    f.apply();
    let mut user = json_of(&target);
    user["providers"]["hub"]["apiKey"] = json!("private-user-key");
    user["providers"]["hub"]["models"][0]["compat"] = json!({"custom": true});
    user["providers"]["hub"]["models"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "custom", "cw": 400}));
    f.put(&target, &serde_json::to_string_pretty(&user).unwrap());
    f.next_generation();
    f.retemplate("pi/models.json", r#"{"providers":{"hub":{"apiKey":"replacement","models":[{"id":"keep","cw":200},{"id":"added","cw":300}]}}}"#);
    f.apply();
    let hub = &json_of(&target)["providers"]["hub"];
    let ids: Vec<&str> = hub["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["keep", "custom", "added"]);
    assert_eq!(
        hub["models"][0],
        json!({"id": "keep", "cw": 200, "compat": {"custom": true}})
    );
    assert_eq!(hub["apiKey"], "private-user-key");
}

#[test]
fn enabled_model_upgrades_keep_roles_comments_and_are_idempotent() {
    let mut f = Fixture::new();
    let target = f.entry(
        "omp/config.yml",
        "omp",
        Some("yaml"),
        "enabledModels: [hub/old, hub/keep]\nmodelRoles: {default: hub/keep}\n",
        json!({"sync": {"kind": "enabled"}}),
    );
    f.apply();
    f.put(&target, "# user\nmodelRoles:\n  default: custom/mine\nenabledModels: [hub/old, hub/keep, custom/mine]\ntheme: mine\n");
    f.next_generation();
    f.retemplate(
        "omp/config.yml",
        "enabledModels: [hub/keep, hub/new]\nmodelRoles: {default: hub/new}\n",
    );
    f.apply();
    let text = read(&target);
    assert!(
        text.starts_with("# user\nmodelRoles:\n  default: custom/mine\n"),
        "{text}"
    );
    assert!(text.contains("theme: mine"));
    let before = fs::read(&target).unwrap();
    assert!(f.apply().writes.is_empty(), "a second run changes nothing");
    assert_eq!(fs::read(&target).unwrap(), before);
    fs::remove_file(&target).unwrap();
    f.apply();
    assert_eq!(read(&target), "enabledModels:\n  - hub/keep\n  - hub/new\n");
}

#[test]
fn preset_upgrades_keep_authentication_and_user_settings() {
    let mut f = Fixture::new();
    let template = r#"{"model":"old","apiKeyHelper":"template","env":{"ANTHROPIC_BASE_URL":"https://template.invalid","CLAUDE_CODE_MAX_CONTEXT_TOKENS":"100","CLAUDE_CODE_MAX_OUTPUT_TOKENS":"20"}}"#;
    let target = f.entry(
        "claude/opus.json",
        "claude-code",
        Some("json"),
        template,
        json!({"seed": "copy", "sync": {"kind": "preset"}}),
    );
    f.apply();
    assert_eq!(
        read(&target),
        template,
        "a new preset is the template, byte for byte"
    );
    f.put(&target, &json!({"model": "old", "apiKeyHelper": "my-helper", "permissions": {"allow": ["mine"]}, "env": {"ANTHROPIC_BASE_URL": "https://mine.invalid", "ANTHROPIC_API_KEY": "private", "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "100", "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "20", "CUSTOM": "mine"}}).to_string());
    f.next_generation();
    f.retemplate("claude/opus.json", r#"{"model":"new","apiKeyHelper":"replacement","env":{"ANTHROPIC_BASE_URL":"https://replacement.invalid","CLAUDE_CODE_MAX_CONTEXT_TOKENS":"200"}}"#);
    f.apply();
    let value = json_of(&target);
    assert_eq!(value["model"], "new");
    assert_eq!(value["env"]["CLAUDE_CODE_MAX_CONTEXT_TOKENS"], "200");
    assert!(value["env"].get("CLAUDE_CODE_MAX_OUTPUT_TOKENS").is_none());
    assert_eq!(value["apiKeyHelper"], "my-helper");
    assert_eq!(value["env"]["ANTHROPIC_API_KEY"], "private");
    assert_eq!(value["env"]["ANTHROPIC_BASE_URL"], "https://mine.invalid");
    assert_eq!(value["permissions"], json!({"allow": ["mine"]}));
}

#[test]
fn toml_presets_update_model_fields_but_not_the_provider() {
    let mut f = Fixture::new();
    let target = f.entry("codex/deep.config.toml", "codex", Some("toml"), "model = \"old\"\nmodel_context_window = 100\nmodel_reasoning_effort = \"low\"\nmodel_provider = \"hub\"\n", json!({"seed": "copy", "sync": {"kind": "preset"}}));
    f.apply();
    f.put(&target, "# mine\nmodel = \"old\"\nmodel_context_window = 100\nmodel_reasoning_effort = \"low\"\nmodel_provider = \"custom\"\n");
    f.next_generation();
    f.retemplate("codex/deep.config.toml", "model = \"new\"\nmodel_context_window = 200\nmodel_reasoning_effort = \"high\"\nmodel_provider = \"replacement\"\n");
    f.apply();
    assert_eq!(
        read(&target),
        "# mine\nmodel = \"new\"\nmodel_context_window = 200\nmodel_reasoning_effort = \"high\"\nmodel_provider = \"custom\"\n"
    );
}

fn patch_sync() -> Value {
    json!({"seed": "copy", "sync": {"kind": "patch", "id": "llm-pi-ai", "file": "cordis.patch.yml", "catalogs": ["models"]}})
}

#[test]
fn bundle_patches_update_models_without_touching_other_profile_data() {
    let mut f = Fixture::new();
    let bundle = f.root.join("gen0/dsh/bundle");
    f.put(&bundle.join("package.json"), r#"{"name":"bundle"}"#);
    f.put(&bundle.join("cordis.patch.yml"), "- id: llm-pi-ai\n  config:\n    providers:\n      hub:\n        models:\n          - id: old\n- id: agent-default-model\n  config:\n    model: mine\n");
    let target = f.home("dsh/bundle");
    let mut entry =
        json!({"name": "dsh/bundle", "harness": "dsh", "source": bundle, "target": target});
    entry
        .as_object_mut()
        .unwrap()
        .extend(patch_sync().as_object().unwrap().clone());
    f.entries.push(entry);
    f.apply();
    assert!(target.is_dir() && !target.is_symlink());
    f.put(&target.join("package.json"), r#"{"name":"custom-package"}"#);
    f.put(&target.join("cordis.patch.yml"), "- id: llm-pi-ai # mine\n  config:\n    providers:\n      hub:\n        apiKeyEnv: MY_KEY\n        models:\n          - id: old\n          - id: custom\n- id: agent-default-model\n  config:\n    model: mine\n- id: custom-plugin\n  config:\n    value: mine\n");
    f.next_generation();
    let next = f.root.join("gen1/dsh/bundle");
    f.put(&next.join("package.json"), r#"{"name":"new-package"}"#);
    f.put(&next.join("cordis.patch.yml"), "- id: llm-pi-ai\n  config:\n    providers:\n      hub:\n        models:\n          - id: new\n            contextWindow: 1000\n- id: agent-default-model\n  config:\n    model: new-default\n");
    f.entries[0]["source"] = json!(next);
    f.apply();
    let text = read(&target.join("cordis.patch.yml"));
    assert!(
        text.contains("# mine")
            && text.contains("apiKeyEnv: MY_KEY")
            && text.contains("model: mine")
            && text.contains("custom-plugin"),
        "{text}"
    );
    assert!(
        text.contains("- id: custom") && text.contains("- id: new") && !text.contains("- id: old"),
        "{text}"
    );
    assert_eq!(
        read(&target.join("package.json")),
        r#"{"name":"custom-package"}"#
    );
}

#[test]
fn a_store_linked_bundle_becomes_writable_and_keeps_non_model_patches() {
    let mut f = Fixture::new();
    let legacy = f.store.join(format!("{}-legacy-bundle", "b".repeat(32)));
    f.put(&legacy.join("package.json"), r#"{"name":"legacy-package"}"#);
    f.put(&legacy.join("cordis.patch.yml"), "- id: llm-pi-ai\n  config:\n    providers:\n      hub:\n        models:\n          - {id: keep, contextWindow: 100}\n- id: agent-default-model\n  config:\n    model: mine\n");
    let target = f.home("dsh/legacy");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&legacy, &target).unwrap();
    let replacement = f.root.join("gen0/dsh/legacy");
    f.put(
        &replacement.join("package.json"),
        r#"{"name":"replacement-package"}"#,
    );
    f.put(&replacement.join("cordis.patch.yml"), "- id: llm-pi-ai\n  config:\n    providers:\n      hub:\n        models:\n          - {id: keep, contextWindow: 200}\n- id: agent-default-model\n  config:\n    model: replacement\n");
    let mut entry =
        json!({"name": "dsh/legacy", "harness": "dsh", "source": replacement, "target": target});
    entry
        .as_object_mut()
        .unwrap()
        .extend(patch_sync().as_object().unwrap().clone());
    f.entries.push(entry);
    // The link predates harnesscfg: the previous generation managed it.
    f.previous = Some(
        f.put(
            &f.root.join("prev.json"),
            &json!({"entries": [{"name": "dsh/legacy", "target": target, "source": legacy}]})
                .to_string(),
        ),
    );
    f.apply();
    assert!(!target.is_symlink());
    let text = read(&target.join("cordis.patch.yml"));
    assert!(
        text.contains("contextWindow: 200") && text.contains("model: mine"),
        "{text}"
    );
    assert_eq!(
        read(&target.join("package.json")),
        r#"{"name":"legacy-package"}"#
    );
    assert!(
        read(&legacy.join("cordis.patch.yml")).contains("contextWindow: 100"),
        "the store copy is untouched"
    );
}

#[test]
fn an_invalid_catalog_blocks_valid_updates_without_disclosing_keys() {
    let mut f = Fixture::new();
    let sync = json!({"sync": {"kind": "providers"}});
    let first = f.entry(
        "pi/valid.json",
        "pi",
        Some("json"),
        r#"{"providers":{"hub":{"models":[{"id":"old"}]}}}"#,
        sync.clone(),
    );
    let invalid = f.entry(
        "pi/invalid.json",
        "pi",
        Some("json"),
        r#"{"providers":{"hub":{"models":[{"id":"old"}]}}}"#,
        sync,
    );
    f.apply();
    f.next_generation();
    f.put(&first, r#"{"providers":{"hub":{"models":[]}}}"#);
    let original = fs::read(&first).unwrap();
    f.put(&invalid, r#"{"providers":{"hub":{"apiKey":"private-key","models":[{"id":"duplicate"},{"id":"duplicate"}]}}}"#);
    let error = f.apply_with(false).err().unwrap();
    assert!(!error.to_string().contains("private-key"));
    assert_eq!(fs::read(&first).unwrap(), original);
}

#[test]
fn every_new_store_reference_gets_its_own_gc_root() {
    let mut f = Fixture::new();
    // A stand-in for `nix-store --realise <path> --add-root <link> --indirect`.
    let fake = f.put(
        &f.root.join("bin/nix-store"),
        "#!/bin/sh\nln -s \"$2\" \"$4\"\n",
    );
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    f.nix_store = Some(fake);
    // The later reference sorts first: numbered roots would collide.
    let (late, early) = (
        f.store.join(format!("{}-a", "a".repeat(32))),
        f.store.join(format!("{}-z", "z".repeat(32))),
    );
    fs::create_dir_all(&late).unwrap();
    fs::create_dir_all(&early).unwrap();
    let target = f.entry(
        "omp/config.yml",
        "omp",
        Some("yaml"),
        &format!(
            "statusLine: {}/bin/x\nenabledModels: [a]\n",
            early.display()
        ),
        json!({"sync": {"kind": "enabled"}}),
    );
    f.apply();
    let roots = f
        .backups()
        .join(format!(
            "nix.{}",
            f.manifest().file_name().unwrap().to_string_lossy()
        ))
        .join("store-roots");
    f.next_generation();
    f.put(
        &target,
        &format!("statusLine: {}/bin/x\nenabledModels: [a]\n", late.display()),
    );
    f.retemplate(
        "omp/config.yml",
        &format!("statusLine: {}/bin/x\nenabledModels: [b]\n", late.display()),
    );
    f.apply();
    let mut found: Vec<PathBuf> = fs::read_dir(&roots)
        .unwrap()
        .map(|e| fs::read_link(e.unwrap().path()).unwrap())
        .collect();
    let second = f
        .backups()
        .join(format!(
            "nix.{}",
            f.manifest().file_name().unwrap().to_string_lossy()
        ))
        .join("store-roots");
    found.extend(
        fs::read_dir(&second)
            .unwrap()
            .map(|e| fs::read_link(e.unwrap().path()).unwrap()),
    );
    found.sort();
    found.dedup();
    assert_eq!(found, [late, early]);
}

#[test]
fn originals_are_backed_up_once_per_generation() {
    let mut f = Fixture::new();
    let target = f.entry(
        "opencode/config.json",
        "opencode",
        Some("json"),
        r#"{"a":1}"#,
        json!({}),
    );
    f.put(&target, r#"{"b":2}"#);
    let report = f.apply();
    let saved = report.backup.unwrap().join("user/opencode/config.json");
    assert_eq!(read(&saved), r#"{"b":2}"#);
    assert_eq!(
        fs::metadata(&saved).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
