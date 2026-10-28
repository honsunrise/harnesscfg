//! Format-neutral merge rules.
//!
//! Two operations exist:
//!
//! * [`fill_missing`] — the first time a file is managed, keys the template has
//!   and the user file lacks are added. Nothing the user wrote changes.
//! * [`sync`] — on every activation, the fields a [`Sync`] spec declares as
//!   Nix-owned follow the template with a three-way merge against the previous
//!   generation's template: fields Nix declared before and no longer declares
//!   are removed, records the user added (custom models, custom providers,
//!   extra env vars) are kept, and everything outside the spec is untouched.
//!
//! Iteration order is deterministic: keys of the previous template first,
//! then new keys in template order.

use serde_json::{Map, Value};

use crate::manifest::Sync;

type Object = Map<String, Value>;

pub type SyncResult<T> = std::result::Result<T, String>;

/// Add template keys missing from `user`, recursing into nested objects.
pub fn fill_missing(user: &mut Object, defaults: &Object) -> bool {
    let mut changed = false;
    for (key, value) in defaults {
        match user.get_mut(key) {
            None => {
                user.insert(key.clone(), value.clone());
                changed = true;
            }
            Some(Value::Object(nested)) => {
                if let Value::Object(defaults) = value {
                    changed |= fill_missing(nested, defaults);
                }
            }
            Some(_) => {}
        }
    }
    changed
}

/// Apply `spec` to `user`, given the template `current` and the previous
/// generation's template `previous` (empty on first sight).
pub fn sync(spec: &Sync, user: &mut Value, current: &Value, previous: &Value) -> SyncResult<()> {
    match spec {
        Sync::Enabled { field } => enabled(user, current, previous, field),
        Sync::Preset {
            keys,
            env_keys,
            env_prefixes,
        } => {
            let owned = |data: &Value| -> SyncResult<Object> {
                let data = object(data)?;
                let mut picked: Object = data
                    .iter()
                    .filter(|(k, _)| keys.contains(k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let env: Object = match data.get("env") {
                    Some(env) => object(env)?
                        .iter()
                        .filter(|(k, _)| {
                            env_keys.contains(k)
                                || env_prefixes.iter().any(|p| k.starts_with(p.as_str()))
                        })
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    None => Object::new(),
                };
                if !env.is_empty() {
                    picked.insert("env".into(), Value::Object(env));
                }
                Ok(picked)
            };
            let (new, old) = (owned(current)?, owned(previous)?);
            sync_owned(object_mut(user)?, &new, &old);
            Ok(())
        }
        Sync::Providers { field, catalogs } => {
            let providers = entry_object(object_mut(user)?, field)?;
            sync_providers(
                providers,
                &child(current, field)?,
                &child(previous, field)?,
                catalogs,
            )
        }
        Sync::Patch {
            id,
            field,
            catalogs,
            ..
        } => {
            let items = user.as_array_mut().ok_or("patch list must be a list")?;
            let new = patch_config(current, id)?.unwrap_or_default();
            let old = patch_config(previous, id)?.unwrap_or_default();
            let index = match patch_index(items, id)? {
                Some(index) => index,
                None => {
                    items.push(serde_json::json!({ "id": id, "config": { field.as_str(): {} } }));
                    items.len() - 1
                }
            };
            let config = entry_object(object_mut(&mut items[index])?, "config")?;
            let providers = entry_object(config, field)?;
            let pick = |config: &Object| -> SyncResult<Object> {
                Ok(config
                    .get(field.as_str())
                    .map(object)
                    .transpose()?
                    .cloned()
                    .unwrap_or_default())
            };
            sync_providers(providers, &pick(&new)?, &pick(&old)?, catalogs)
        }
    }
}

fn enabled(user: &mut Value, current: &Value, previous: &Value, field: &str) -> SyncResult<()> {
    let list = |value: &Value| -> SyncResult<Vec<String>> {
        match object(value)?.get(field) {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| format!("{field} must list strings"))
                })
                .collect(),
            Some(_) => Err(format!("{field} must list strings")),
        }
    };
    let (new, old, selected) = (list(current)?, list(previous)?, list(user)?);
    let managed = |item: &String| new.contains(item) || old.contains(item);
    let mut result: Vec<Value> = selected
        .into_iter()
        .filter(|item| !managed(item))
        .map(Value::String)
        .collect();
    result.extend(new.into_iter().map(Value::String));
    object_mut(user)?.insert(field.to_owned(), Value::Array(result));
    Ok(())
}

/// Template-declared fields belong to Nix; unknown fields survive.
pub fn sync_owned(user: &mut Object, current: &Object, previous: &Object) {
    for key in union(previous, current) {
        match current.get(&key) {
            Some(value) => match (value, user.get_mut(&key)) {
                (Value::Object(value), Some(Value::Object(nested))) => {
                    let previous = previous
                        .get(&key)
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    sync_owned(nested, value, &previous);
                }
                _ => {
                    user.insert(key, value.clone());
                }
            },
            None => match (&previous[&key], user.get_mut(&key)) {
                (Value::Object(previous), Some(Value::Object(nested))) => {
                    sync_owned(nested, &Object::new(), previous);
                    if nested.is_empty() {
                        user.shift_remove(&key);
                    }
                }
                _ => {
                    user.shift_remove(&key);
                }
            },
        }
    }
}

/// A model catalog: a list of records with unique string `id`s, or an object
/// keyed by id. Records Nix retired disappear; custom records stay.
fn sync_catalog(
    user: Option<&Value>,
    current: Option<&Value>,
    previous: Option<&Value>,
) -> SyncResult<Value> {
    let sequence = matches!(current.or(previous), Some(Value::Array(_)));
    let records = |value: Option<&Value>| -> SyncResult<Map<String, Value>> {
        let Some(value) = value else {
            return Ok(Map::new());
        };
        if !sequence {
            return Ok(object(value)?.clone());
        }
        let items = value.as_array().ok_or("model catalog must be a list")?;
        let mut result = Map::new();
        for item in items {
            object(item)?;
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty());
            let id = id.ok_or("model id missing")?;
            if result.insert(id.to_owned(), item.clone()).is_some() {
                return Err(format!("duplicate model id {id}"));
            }
        }
        Ok(result)
    };
    let (mut result, new, old) = (records(user)?, records(current)?, records(previous)?);
    for id in old.keys().filter(|id| !new.contains_key(*id)) {
        result.shift_remove(id);
    }
    for (id, value) in &new {
        let record = result
            .entry(id.clone())
            .or_insert_with(|| Value::Object(Object::new()));
        let old = old
            .get(id)
            .map(object)
            .transpose()?
            .cloned()
            .unwrap_or_default();
        sync_owned(object_mut(record)?, object(value)?, &old);
    }
    Ok(if sequence {
        Value::Array(result.into_iter().map(|(_, v)| v).collect())
    } else {
        Value::Object(result)
    })
}

fn sync_providers(
    user: &mut Object,
    current: &Object,
    previous: &Object,
    catalogs: &[String],
) -> SyncResult<()> {
    for name in union(previous, current) {
        let new = current
            .get(&name)
            .map(object)
            .transpose()?
            .cloned()
            .unwrap_or_default();
        let old = previous
            .get(&name)
            .map(object)
            .transpose()?
            .cloned()
            .unwrap_or_default();
        let fields: Vec<&String> = catalogs
            .iter()
            .filter(|f| new.contains_key(*f) || old.contains_key(*f))
            .collect();
        if fields.is_empty() {
            continue;
        }
        let Some(provider) = user.get_mut(&name) else {
            if current.contains_key(&name) {
                for field in &fields {
                    sync_catalog(None, new.get(*field), old.get(*field))?; // Validate before copying.
                }
                user.insert(name, Value::Object(new));
            }
            continue;
        };
        let provider = object_mut(provider)?;
        for field in fields {
            let value = sync_catalog(provider.get(field), new.get(field), old.get(field))?;
            let empty = matches!(&value, Value::Array(a) if a.is_empty())
                || matches!(&value, Value::Object(o) if o.is_empty());
            if !new.contains_key(field) && empty {
                provider.shift_remove(field);
            } else {
                provider.insert(field.clone(), value);
            }
        }
    }
    Ok(())
}

fn patch_index(items: &[Value], id: &str) -> SyncResult<Option<usize>> {
    let mut found = None;
    for (index, item) in items.iter().enumerate() {
        if object(item)?.get("id").and_then(Value::as_str) == Some(id) {
            if found.is_some() {
                return Err(format!("duplicate patch {id}"));
            }
            found = Some(index);
        }
    }
    Ok(found)
}

fn patch_config(list: &Value, id: &str) -> SyncResult<Option<Object>> {
    let items = list.as_array().ok_or("patch list must be a list")?;
    Ok(match patch_index(items, id)? {
        Some(index) => Some(
            items[index]
                .get("config")
                .map(object)
                .transpose()?
                .cloned()
                .unwrap_or_default(),
        ),
        None => None,
    })
}

fn union(first: &Object, second: &Object) -> Vec<String> {
    first
        .keys()
        .chain(second.keys().filter(|k| !first.contains_key(*k)))
        .cloned()
        .collect()
}

fn object(value: &Value) -> SyncResult<&Object> {
    value
        .as_object()
        .ok_or_else(|| "model configuration must be an object".into())
}

fn object_mut(value: &mut Value) -> SyncResult<&mut Object> {
    value
        .as_object_mut()
        .ok_or_else(|| "model configuration must be an object".into())
}

fn child(value: &Value, field: &str) -> SyncResult<Object> {
    Ok(object(value)?
        .get(field)
        .map(object)
        .transpose()?
        .cloned()
        .unwrap_or_default())
}

/// `setdefault(field, {})`, as the clients store an empty object there too.
fn entry_object<'a>(map: &'a mut Object, field: &str) -> SyncResult<&'a mut Object> {
    object_mut(
        map.entry(field.to_owned())
            .or_insert_with(|| Value::Object(Object::new())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn providers() -> Sync {
        Sync::Providers {
            field: "providers".into(),
            catalogs: vec!["models".into(), "modelOverrides".into()],
        }
    }

    #[test]
    fn fill_missing_keeps_user_values_and_adds_nested_defaults() {
        let mut user = json!({"model": "mine", "plugins": [], "providers": {"custom": {}}});
        let defaults = json!({"model": "new", "providers": {"hub": {}}, "plugins": ["default"]});
        assert!(fill_missing(
            user.as_object_mut().unwrap(),
            defaults.as_object().unwrap()
        ));
        assert_eq!(
            user,
            json!({"model": "mine", "plugins": [], "providers": {"custom": {}, "hub": {}}})
        );
    }

    #[test]
    fn catalogs_update_owned_fields_retire_old_ids_and_keep_custom_records() {
        for (old_models, new_models, custom) in [
            (
                json!([{"id": "keep", "cw": 100, "obsolete": true}, {"id": "retire"}]),
                json!([{"id": "keep", "cw": 200}, {"id": "added", "cw": 300}]),
                json!({"id": "custom", "cw": 400}),
            ),
            (
                json!({"keep": {"cw": 100, "obsolete": true}, "retire": {}}),
                json!({"keep": {"cw": 200}, "added": {"cw": 300}}),
                json!({"cw": 400}),
            ),
        ] {
            let field = if old_models.is_array() {
                "models"
            } else {
                "modelOverrides"
            };
            let previous = json!({"providers": {"hub": {"apiKey": "template", field: old_models}}});
            let mut user = previous.clone();
            user["providers"]["hub"]["apiKey"] = json!("private");
            match &mut user["providers"]["hub"][field] {
                Value::Array(items) => {
                    items[0]["compat"] = json!({"custom": true});
                    items.push(custom.clone());
                }
                Value::Object(map) => {
                    map["keep"]["compat"] = json!({"custom": true});
                    map.insert("custom".into(), custom.clone());
                }
                _ => unreachable!(),
            }
            let current =
                json!({"providers": {"hub": {"apiKey": "replacement", field: new_models}}});
            sync(&providers(), &mut user, &current, &previous).unwrap();
            let hub = &user["providers"]["hub"];
            assert_eq!(hub["apiKey"], "private");
            let records: Map<String, Value> = match &hub[field] {
                Value::Array(items) => items
                    .iter()
                    .map(|i| (i["id"].as_str().unwrap().to_owned(), i.clone()))
                    .collect(),
                Value::Object(map) => map.clone(),
                _ => unreachable!(),
            };
            assert_eq!(
                records.keys().collect::<Vec<_>>(),
                ["keep", "custom", "added"]
            );
            assert_eq!(records["keep"]["cw"], 200);
            assert!(records["keep"].get("obsolete").is_none());
            assert_eq!(records["keep"]["compat"], json!({"custom": true}));
            assert_eq!(records["custom"]["cw"], 400);
        }
    }

    #[test]
    fn new_providers_need_their_full_declaration() {
        let mut user = json!({"providers": {}});
        sync(
            &providers(),
            &mut user,
            &json!({"providers": {"hub": {"baseUrl": "u", "models": [{"id": "a"}]}}}),
            &json!({}),
        )
        .unwrap();
        assert_eq!(
            user,
            json!({"providers": {"hub": {"baseUrl": "u", "models": [{"id": "a"}]}}})
        );
    }

    #[test]
    fn retired_catalog_fields_disappear_when_empty() {
        let mut user = json!({"providers": {"hub": {"models": [{"id": "a"}], "baseUrl": "mine"}}});
        sync(
            &providers(),
            &mut user,
            &json!({"providers": {"hub": {}}}),
            &json!({"providers": {"hub": {"models": [{"id": "a"}]}}}),
        )
        .unwrap();
        assert_eq!(user, json!({"providers": {"hub": {"baseUrl": "mine"}}}));
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let mut user = json!({"providers": {"hub": {"models": [{"id": "d"}, {"id": "d"}]}}});
        let template = json!({"providers": {"hub": {"models": [{"id": "x"}]}}});
        assert!(sync(&providers(), &mut user, &template, &json!({})).is_err());
    }

    #[test]
    fn enabled_lists_keep_custom_entries_first() {
        let spec = Sync::Enabled {
            field: "enabledModels".into(),
        };
        let mut user = json!({"enabledModels": ["hub/old", "hub/keep", "custom/mine"], "modelRoles": {"default": "custom/mine"}});
        sync(
            &spec,
            &mut user,
            &json!({"enabledModels": ["hub/keep", "hub/new"]}),
            &json!({"enabledModels": ["hub/old", "hub/keep"]}),
        )
        .unwrap();
        assert_eq!(
            user,
            json!({"enabledModels": ["custom/mine", "hub/keep", "hub/new"], "modelRoles": {"default": "custom/mine"}})
        );
    }

    #[test]
    fn presets_own_only_model_fields_and_model_env() {
        let spec = Sync::preset_default();
        let previous = json!({"model": "old", "apiKeyHelper": "t", "env": {"CLAUDE_CODE_MAX_CONTEXT_TOKENS": "100", "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "20", "ANTHROPIC_BASE_URL": "t"}});
        let mut user = json!({"model": "old", "apiKeyHelper": "mine", "env": {"CLAUDE_CODE_MAX_CONTEXT_TOKENS": "100", "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "20", "ANTHROPIC_BASE_URL": "mine", "CUSTOM": "x"}});
        let current = json!({"model": "new", "apiKeyHelper": "r", "env": {"CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200", "ANTHROPIC_BASE_URL": "r"}});
        sync(&spec, &mut user, &current, &previous).unwrap();
        assert_eq!(
            user,
            json!({"model": "new", "apiKeyHelper": "mine", "env": {"CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200", "ANTHROPIC_BASE_URL": "mine", "CUSTOM": "x"}})
        );
    }

    #[test]
    fn patch_lists_sync_only_their_providers() {
        let spec = Sync::Patch {
            id: "llm-pi-ai".into(),
            file: "cordis.patch.yml".into(),
            field: "providers".into(),
            catalogs: vec!["models".into()],
        };
        let mut user = json!([{"id": "llm-pi-ai", "config": {"providers": {"hub": {"apiKeyEnv": "K", "models": [{"id": "old"}, {"id": "custom"}]}}}}, {"id": "other", "config": {"v": 1}}]);
        let previous = json!([{"id": "llm-pi-ai", "config": {"providers": {"hub": {"models": [{"id": "old"}]}}}}]);
        let current = json!([{"id": "llm-pi-ai", "config": {"providers": {"hub": {"models": [{"id": "new"}]}}}}, {"id": "agent-default-model", "config": {"model": "x"}}]);
        sync(&spec, &mut user, &current, &previous).unwrap();
        assert_eq!(
            user[0]["config"]["providers"]["hub"],
            json!({"apiKeyEnv": "K", "models": [{"id": "custom"}, {"id": "new"}]})
        );
        assert_eq!(user[1], json!({"id": "other", "config": {"v": 1}}));
        assert_eq!(user.as_array().unwrap().len(), 2);
    }
}
