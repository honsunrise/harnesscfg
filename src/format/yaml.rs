//! YAML through `yaml-edit`'s lossless syntax tree.
//!
//! Plain scalars resolve with the YAML 1.2 core schema, as OMP and DSH read
//! them: `off`/`yes` stay strings and `0123` is the integer 123. Anchors,
//! aliases, tags and multi-document files are rejected rather than guessed.
//! New values are emitted as YAML text, re-parsed and spliced in, so their
//! quoting is decided here, not by the library.

use std::str::FromStr;

use serde_json::{Map, Number, Value};
use yaml_edit::{Document, Mapping, Scalar, Sequence, YamlFile, YamlNode};

use super::{Root, split};
use crate::diff::{Op, Seg};

pub struct Doc(YamlFile);

impl Doc {
    pub fn parse(text: &str, root: Root) -> Option<(Doc, Value)> {
        let file = YamlFile::from_str(text).ok()?;
        let mut documents = file.documents();
        let first = documents.next();
        if documents.next().is_some() {
            return None;
        }
        let value = match &first {
            Some(doc) => document(doc)?,
            None => Value::Null,
        };
        if first.is_none() || value.is_null() {
            if root == Root::Array {
                // `ensure_document` only creates mappings; seed a sequence.
                return Some((
                    Doc(YamlFile::from_str(&format!("{text}[]\n")).ok()?),
                    Value::Null,
                ));
            }
            file.ensure_document();
        }
        Some((Doc(file), value))
    }

    pub fn apply(&mut self, ops: &[Op]) -> Result<(), String> {
        let doc = self.0.document().ok_or("document has no root")?;
        for op in ops {
            let (parent, last) = split(op);
            let mut node = match (doc.as_mapping(), doc.as_sequence()) {
                (Some(m), _) => Container::Mapping(m),
                (_, Some(s)) => Container::Sequence(s),
                _ => return Err("expected a mapping or a sequence root".into()),
            };
            for seg in parent {
                node = node.child(seg)?;
            }
            node.apply(op, last)?;
        }
        Ok(())
    }

    pub fn render(&self) -> String {
        self.0.to_string()
    }
}

enum Container {
    Mapping(Mapping),
    Sequence(Sequence),
}

impl Container {
    fn child(self, seg: &Seg) -> Result<Container, String> {
        let node = match (self, seg) {
            (Container::Mapping(m), Seg::Key(k)) => m.get(k.as_str()),
            (Container::Sequence(s), Seg::Index(i)) => s.get(*i),
            _ => None,
        };
        match node.ok_or_else(|| format!("missing {seg:?}"))? {
            YamlNode::Mapping(m) => Ok(Container::Mapping(m)),
            YamlNode::Sequence(s) => Ok(Container::Sequence(s)),
            _ => Err("expected a mapping or a sequence".into()),
        }
    }

    fn apply(self, op: &Op, last: &Seg) -> Result<(), String> {
        match (self, op, last) {
            (Container::Mapping(m), Op::Set(_, v), Seg::Key(k)) => m.set(k.as_str(), node(v)?),
            (Container::Mapping(m), Op::Remove(_), Seg::Key(k)) => {
                m.remove(k.as_str()).ok_or("missing key")?;
            }
            (Container::Sequence(s), Op::Set(_, v), Seg::Index(i)) => {
                if !s.set(*i, node(v)?) {
                    return Err("index out of range".into());
                }
            }
            (Container::Sequence(s), Op::Remove(_), Seg::Index(i)) => {
                s.remove(*i).ok_or("index out of range")?;
            }
            (Container::Sequence(s), Op::Insert(_, v), Seg::Index(i)) => {
                if *i == s.len() {
                    s.push(node(v)?)
                } else {
                    s.insert(*i, node(v)?)
                }
            }
            _ => return Err("op does not fit its target".into()),
        }
        Ok(())
    }
}

/// A detached node holding `value`, parsed from our own emitted text.
fn node(value: &Value) -> Result<YamlNode, String> {
    let mut lines = Vec::new();
    entry(&mut lines, 0, "v:", value);
    let doc = Document::from_str(&(lines.join("\n") + "\n"))
        .map_err(|_| "cannot build a YAML node".to_string())?;
    doc.as_mapping()
        .and_then(|m| m.get("v"))
        .ok_or_else(|| "cannot build a YAML node".into())
}

/// A whole document, for files created from scratch.
pub fn emit(value: &Value) -> String {
    let mut lines = Vec::new();
    if collection(value) {
        body(&mut lines, 0, value);
    } else {
        lines.push(scalar_text(value));
    }
    lines.join("\n") + "\n"
}

fn collection(value: &Value) -> bool {
    matches!(value, Value::Array(a) if !a.is_empty())
        || matches!(value, Value::Object(o) if !o.is_empty())
}

/// `head` (`key:` or `-`) followed by `value`: inline for scalars and empty
/// collections, as an indented block otherwise.
fn entry(lines: &mut Vec<String>, depth: usize, head: &str, value: &Value) {
    let pad = "  ".repeat(depth);
    if collection(value) {
        lines.push(format!("{pad}{head}"));
        body(lines, depth + 1, value);
    } else {
        lines.push(format!("{pad}{head} {}", scalar_text(value)));
    }
}

fn body(lines: &mut Vec<String>, depth: usize, value: &Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                entry(lines, depth, &format!("{}:", string_text(key)), value);
            }
        }
        Value::Array(items) => {
            for item in items {
                entry(lines, depth, "-", item);
            }
        }
        _ => unreachable!("body is only called on collections"),
    }
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => string_text(s),
        Value::Array(_) => "[]".into(),
        Value::Object(_) => "{}".into(),
    }
}

/// Plain when the core schema would read it back as the same string,
/// double-quoted (JSON escaping is valid YAML) otherwise.
fn string_text(s: &str) -> String {
    let plain = !s.is_empty()
        && !s.starts_with(|c: char| c.is_whitespace() || "-?:,[]{}#&*!|>'\"%@`".contains(c))
        && !s.ends_with(|c: char| c.is_whitespace() || c == ':')
        && !s.contains(": ")
        && !s.contains(" #")
        && !s.chars().any(|c| c.is_control() || ",[]{}".contains(c))
        && matches!(plain_scalar(s), Value::String(_));
    if plain {
        s.to_owned()
    } else {
        serde_json::to_string(s).expect("strings always serialize")
    }
}

fn document(doc: &Document) -> Option<Value> {
    if let Some(m) = doc.as_mapping() {
        return mapping(&m);
    }
    if let Some(s) = doc.as_sequence() {
        return sequence(&s);
    }
    if let Some(s) = doc.as_scalar() {
        return scalar(&s);
    }
    Some(Value::Null)
}

fn value(node: &YamlNode) -> Option<Value> {
    match node {
        YamlNode::Scalar(s) => scalar(s),
        YamlNode::Mapping(m) => mapping(m),
        YamlNode::Sequence(s) => sequence(s),
        YamlNode::Alias(_) | YamlNode::TaggedNode(_) => None,
    }
}

fn mapping(m: &Mapping) -> Option<Value> {
    let mut map = Map::new();
    for (key, val) in m.iter() {
        let key = match value(&key)? {
            Value::String(s) => s,
            Value::Null => return None,
            other => other.to_string(),
        };
        if map.insert(key, value(&val)?).is_some() {
            return None; // Duplicate keys: which one wins is client-specific.
        }
    }
    Some(Value::Object(map))
}

fn sequence(s: &Sequence) -> Option<Value> {
    s.values()
        .map(|item| value(&item))
        .collect::<Option<Vec<_>>>()
        .map(Value::Array)
}

fn scalar(s: &Scalar) -> Option<Value> {
    let raw = s.value();
    if raw.starts_with('&') || raw.starts_with('!') {
        return None;
    }
    if s.is_quoted() || s.is_block() {
        return Some(Value::String(s.as_string()));
    }
    Some(plain_scalar(raw.trim()))
}

/// YAML 1.2 core schema resolution of a plain scalar.
fn plain_scalar(s: &str) -> Value {
    match s {
        "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    let digits = |body: &str, radix| !body.is_empty() && i64::from_str_radix(body, radix).is_ok();
    if let Some(hex) = s.strip_prefix("0x").filter(|b| digits(b, 16)) {
        return Value::Number(i64::from_str_radix(hex, 16).unwrap().into());
    }
    if let Some(oct) = s.strip_prefix("0o").filter(|b| digits(b, 8)) {
        return Value::Number(i64::from_str_radix(oct, 8).unwrap().into());
    }
    let unsigned = s.strip_prefix(['-', '+']).unwrap_or(s);
    if !unsigned.is_empty()
        && unsigned.bytes().all(|b| b.is_ascii_digit())
        && let Ok(i) = s.parse::<i64>()
    {
        return Value::Number(i.into());
    }
    let float = !unsigned.is_empty()
        && unsigned
            .bytes()
            .all(|b| b.is_ascii_digit() || b"._eE+-".contains(&b))
        && unsigned
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_digit() || b == b'.')
        && unsigned.bytes().any(|b| b.is_ascii_digit());
    if float && let Some(n) = s.parse::<f64>().ok().and_then(Number::from_f64) {
        return Value::Number(n);
    }
    Value::String(s.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_schema_matches_yaml_12() {
        for (text, expected) in [
            ("off", Value::from("off")),
            ("yes", Value::from("yes")),
            ("0123", Value::from(123)),
            ("0x1F", Value::from(31)),
            ("-1.5e3", Value::from(-1500.0)),
            ("~", Value::Null),
            ("True", Value::Bool(true)),
            ("1.2.3", Value::from("1.2.3")),
            ("hub/new", Value::from("hub/new")),
        ] {
            assert_eq!(plain_scalar(text), expected, "{text}");
        }
    }

    #[test]
    fn emitted_strings_read_back_unchanged() {
        for s in [
            "on",
            "off",
            "yes",
            "null",
            "123",
            "1.5",
            "",
            " pad",
            "a: b",
            "a #b",
            "#x",
            "-x",
            "x:",
            "[a]",
            "quote\"d",
            "multi\nline",
            "中文",
            "hub/new",
            "plain words",
        ] {
            let doc = Document::from_str(&format!("k: {}\n", string_text(s))).unwrap();
            let back = value(&doc.as_mapping().unwrap().get("k").unwrap()).unwrap();
            assert_eq!(back, Value::from(s), "{s:?} emitted as {}", string_text(s));
        }
        assert_eq!(string_text("hub/new"), "hub/new");
    }

    #[test]
    fn anchors_aliases_tags_and_multiple_documents_are_refused() {
        for text in ["a: &x 1\nb: *x\n", "a: !!str 1\n", "a: 1\n---\nb: 2\n"] {
            assert!(Doc::parse(text, Root::Object).is_none(), "{text}");
        }
    }
}
