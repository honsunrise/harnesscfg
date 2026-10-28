//! Lossless documents for every supported format.
//!
//! An [`Edit`] owns one parsed file and the value it currently represents.
//! Callers compute a new value with format-neutral merge logic, then call
//! [`Edit::update`]: the backend replays the [`diff`](crate::diff) on its
//! syntax tree, so comments, quoting and layout outside the changed nodes
//! survive byte for byte.

mod json;
mod toml;
mod yaml;

use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::diff::{self, Op};
use crate::error::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// JSON, also accepting comments and trailing commas (JSONC).
    Json,
    Toml,
    Yaml,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Json => "JSON",
            Format::Toml => "TOML",
            Format::Yaml => "YAML",
        }
    }
}

/// Which root a document must have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Root {
    Object,
    Array,
}

impl Root {
    fn empty(self) -> Value {
        match self {
            Root::Object => Value::Object(Default::default()),
            Root::Array => Value::Array(Vec::new()),
        }
    }

    fn accepts(self, value: &Value) -> bool {
        matches!(
            (self, value),
            (Root::Object, Value::Object(_)) | (Root::Array, Value::Array(_))
        )
    }
}

enum Doc {
    Json(json::Doc),
    Toml(toml::Doc),
    Yaml(yaml::Doc),
    /// No source text: rendered from the value with the format's emitter.
    Fresh,
}

pub struct Edit {
    format: Format,
    doc: Doc,
    value: Value,
}

impl Edit {
    /// Parse `raw`. Blank input (empty, whitespace or comments only) yields an
    /// empty root and keeps its text, so comments in otherwise empty files stay.
    pub fn parse(format: Format, root: Root, raw: &[u8], path: &Path) -> Result<Edit> {
        let fail = || Error::Parse {
            path: path.to_path_buf(),
            format: format.name(),
        };
        let text = std::str::from_utf8(raw).map_err(|_| fail())?;
        let (doc, value) = match format {
            Format::Json => json::Doc::parse(text, root).map(|(d, v)| (Doc::Json(d), v)),
            Format::Toml => toml::Doc::parse(text).map(|(d, v)| (Doc::Toml(d), v)),
            Format::Yaml => yaml::Doc::parse(text, root).map(|(d, v)| (Doc::Yaml(d), v)),
        }
        .ok_or_else(fail)?;
        let value = if value.is_null() { root.empty() } else { value };
        if !root.accepts(&value) {
            return Err(fail());
        }
        Ok(Edit { format, doc, value })
    }

    /// A document with no file behind it yet.
    pub fn empty(format: Format, root: Root) -> Edit {
        Edit {
            format,
            doc: Doc::Fresh,
            value: root.empty(),
        }
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Change the document to represent `new`. Returns whether anything changed.
    pub fn update(&mut self, new: Value, path: &Path) -> Result<bool> {
        if new == self.value {
            return Ok(false);
        }
        let ops = diff::diff(&self.value, &new);
        let unrepresentable = |reason: String| Error::Unrepresentable {
            format: self.format.name(),
            reason,
        };
        let replayed = match &mut self.doc {
            Doc::Json(doc) => doc.apply(&ops),
            Doc::Toml(doc) => doc.apply(&ops),
            Doc::Yaml(doc) => doc.apply(&ops),
            Doc::Fresh => Ok(()),
        };
        replayed.map_err(|reason| unrepresentable(format!("{reason} in {}", path.display())))?;
        self.value = new;
        Ok(true)
    }

    pub fn render(&self) -> Result<Vec<u8>> {
        let text = match &self.doc {
            Doc::Json(doc) => doc.render(),
            Doc::Toml(doc) => doc.render(),
            Doc::Yaml(doc) => doc.render(),
            Doc::Fresh => match self.format {
                Format::Json => json::emit(&self.value),
                Format::Toml => {
                    toml::emit(&self.value).map_err(|reason| Error::Unrepresentable {
                        format: "TOML",
                        reason,
                    })?
                }
                Format::Yaml => yaml::emit(&self.value),
            },
        };
        Ok(text.into_bytes())
    }
}

/// Split an op into its parent path and final segment.
fn split(op: &Op) -> (&[diff::Seg], &diff::Seg) {
    let path = match op {
        Op::Set(path, _) | Op::Remove(path) | Op::Insert(path, _) => path,
    };
    let (last, parent) = path.split_last().expect("diff never emits root ops");
    (parent, last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every backend must agree with the reference replay and keep the rest
    /// of the text verbatim.
    fn check(format: Format, root: Root, before: &str, new: Value) -> String {
        let path = Path::new("fixture");
        let mut edit = Edit::parse(format, root, before.as_bytes(), path).unwrap();
        edit.update(new.clone(), path).unwrap();
        let out = String::from_utf8(edit.render().unwrap()).unwrap();
        let reparsed = Edit::parse(format, root, out.as_bytes(), path).unwrap();
        assert_eq!(reparsed.value(), &new, "rendered:\n{out}");
        out
    }

    #[test]
    fn json_keeps_comments_and_untouched_layout() {
        let out = check(
            Format::Json,
            Root::Object,
            "{\n  // mine\n  \"keep\": 1, /* inline */\n  \"models\": [{\"id\": \"a\", \"cw\": 1}],\n  \"drop\": true,\n}\n",
            json!({"keep": 1, "models": [{"id": "a", "cw": 2}, {"id": "b"}], "added": {"x": [1, "two"]}}),
        );
        assert!(out.contains("// mine"));
        assert!(out.contains("/* inline */"));
    }

    #[test]
    fn toml_keeps_comments_tables_and_inline_comments() {
        let out = check(
            Format::Toml,
            Root::Object,
            "# mine\nmodel = \"old\" # trailing\n\n[projects.\"/work\"]\ntrust_level = \"trusted\"\n",
            json!({"model": "new", "projects": {"/work": {"trust_level": "trusted"}}, "mcp_servers": {"new": {"command": "x", "args": ["a"]}}}),
        );
        assert!(
            out.starts_with("# mine\nmodel = \"new\" # trailing\n"),
            "{out}"
        );
        assert!(out.contains("[projects.\"/work\"]"));
    }

    #[test]
    fn yaml_keeps_comments_and_yaml_12_scalars() {
        let out = check(
            Format::Yaml,
            Root::Object,
            "# mine\nthinking: off\nquoted: \"on\"\nnumber: 0123\nenabledModels: [hub/old, hub/keep]\nproviders:\n  hub:\n    apiKey: k # secret\n    models:\n      - id: keep # kept\n        cw: 100\n      - id: retire\n",
            json!({
                "thinking": "off", "quoted": "on", "number": 123,
                "enabledModels": ["hub/keep", "hub/new"],
                "providers": {"hub": {"apiKey": "k", "models": [{"id": "keep", "cw": 200}, {"id": "added", "name": "on", "tags": []}]}},
            }),
        );
        for kept in [
            "# mine",
            "thinking: off",
            "quoted: \"on\"",
            "number: 0123",
            "# secret",
            "# kept",
        ] {
            assert!(out.contains(kept), "{kept} missing from:\n{out}");
        }
    }

    #[test]
    fn yaml_patch_lists_are_array_roots() {
        check(
            Format::Yaml,
            Root::Array,
            "# patches\n- id: llm-pi-ai\n  config:\n    providers: {}\n- id: other # mine\n",
            json!([{"id": "llm-pi-ai", "config": {"providers": {"hub": {"models": [{"id": "m"}]}}}}, {"id": "other"}]),
        );
    }

    #[test]
    fn blank_documents_keep_their_comments() {
        for format in [Format::Json, Format::Toml, Format::Yaml] {
            let before = if format == Format::Json {
                "// only\n"
            } else {
                "# only\n"
            };
            let out = check(
                format,
                Root::Object,
                before,
                json!({"a": {"b": [1, 2]}, "s": "x"}),
            );
            assert!(out.contains("only"), "{format:?}: {out}");
        }
    }

    #[test]
    fn fresh_documents_render_from_values() {
        let value = json!({"a": {"b": [1, {"c": "yes"}]}, "empty": {}, "s": "on", "n": 1.5});
        for format in [Format::Json, Format::Toml, Format::Yaml] {
            let mut edit = Edit::empty(format, Root::Object);
            edit.update(value.clone(), Path::new("fresh")).unwrap();
            let out = edit.render().unwrap();
            let reparsed = Edit::parse(format, Root::Object, &out, Path::new("fresh")).unwrap();
            assert_eq!(reparsed.value(), &value, "{format:?}");
        }
    }

    #[test]
    fn parse_errors_never_quote_content() {
        for format in [Format::Json, Format::Toml, Format::Yaml] {
            let error = Edit::parse(format, Root::Object, b"secret-value: [}{", Path::new("p"))
                .err()
                .unwrap();
            assert!(
                !error.to_string().contains("secret-value"),
                "{format:?}: {error}"
            );
        }
    }
}
