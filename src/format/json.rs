//! JSON and JSONC through `jsonc-parser`'s concrete syntax tree. Comments and
//! trailing commas are accepted and kept.

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstNode, CstRootNode};
use serde_json::Value;

use super::{Root, split};
use crate::diff::{Op, Seg};

pub struct Doc(CstRootNode);

impl Doc {
    pub fn parse(text: &str, root: Root) -> Option<(Doc, Value)> {
        let cst = CstRootNode::parse(text, &ParseOptions::default()).ok()?;
        let value = match cst.value() {
            Some(node) => node.to_serde_value()?,
            None => Value::Null,
        };
        if value.is_null() {
            // Blank (or comment-only) file: give it the required root so ops
            // have somewhere to land; the comments stay.
            match root {
                Root::Object => drop(cst.object_value_or_set()),
                Root::Array => cst.set_value(CstInputValue::Array(Vec::new())),
            }
        }
        Some((Doc(cst), value))
    }

    pub fn apply(&mut self, ops: &[Op]) -> Result<(), String> {
        for op in ops {
            let (parent, last) = split(op);
            let node = navigate(self.0.value().ok_or("document has no root")?, parent)?;
            match (op, last) {
                (Op::Set(_, value), Seg::Key(key)) => {
                    let object = node.as_object().ok_or("expected an object")?;
                    match object.get(key) {
                        Some(prop) => prop.set_value(input(value)),
                        None => drop(object.append(key, input(value))),
                    }
                }
                (Op::Remove(_), Seg::Key(key)) => {
                    let object = node.as_object().ok_or("expected an object")?;
                    object.get(key).ok_or("missing key")?.remove();
                }
                (Op::Set(_, value), Seg::Index(index)) => {
                    let array = node.as_array().ok_or("expected an array")?;
                    array
                        .elements()
                        .get(*index)
                        .ok_or("index out of range")?
                        .clone()
                        .remove();
                    array.insert(*index, input(value));
                }
                (Op::Remove(_), Seg::Index(index)) => {
                    let array = node.as_array().ok_or("expected an array")?;
                    array
                        .elements()
                        .get(*index)
                        .ok_or("index out of range")?
                        .clone()
                        .remove();
                }
                (Op::Insert(_, value), Seg::Index(index)) => {
                    node.as_array()
                        .ok_or("expected an array")?
                        .insert(*index, input(value));
                }
                (Op::Insert(..), Seg::Key(_)) => return Err("insert needs an index".into()),
            }
        }
        Ok(())
    }

    pub fn render(&self) -> String {
        self.0.to_string()
    }
}

fn navigate(mut node: CstNode, path: &[Seg]) -> Result<CstNode, String> {
    for seg in path {
        node = match seg {
            Seg::Key(key) => node
                .as_object()
                .and_then(|object| object.get(key))
                .and_then(|prop| prop.value())
                .ok_or_else(|| format!("missing key {key}"))?,
            Seg::Index(index) => node
                .as_array()
                .and_then(|array| array.elements().get(*index).cloned())
                .ok_or_else(|| format!("missing index {index}"))?,
        };
    }
    Ok(node)
}

fn input(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(b) => CstInputValue::Bool(*b),
        Value::Number(n) => CstInputValue::Number(n.to_string()),
        Value::String(s) => CstInputValue::String(s.clone()),
        Value::Array(items) => CstInputValue::Array(items.iter().map(input).collect()),
        Value::Object(map) => {
            CstInputValue::Object(map.iter().map(|(k, v)| (k.clone(), input(v))).collect())
        }
    }
}

/// Pretty JSON with a trailing newline, for files created from scratch.
pub fn emit(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("JSON values always serialize");
    text.push('\n');
    text
}
