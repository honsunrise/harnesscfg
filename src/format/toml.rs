//! TOML through `toml_edit`, which keeps comments, key order and table layout.
//!
//! Datetimes have no JSON counterpart; they read as their TOML string form.
//! Merge logic only copies values out of templates, so a datetime is only
//! affected when Nix declares the same key.

use serde_json::{Map, Number, Value};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike};

use super::split;
use crate::diff::{Op, Seg};

pub struct Doc(DocumentMut);

impl Doc {
    pub fn parse(text: &str) -> Option<(Doc, Value)> {
        let doc: DocumentMut = text.parse().ok()?;
        let value = table(doc.as_table());
        Some((Doc(doc), value))
    }

    pub fn apply(&mut self, ops: &[Op]) -> Result<(), String> {
        for op in ops {
            let (parent, last) = split(op);
            let mut node = Node::Table(self.0.as_table_mut());
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

/// A container reachable by path. Standard tables, inline tables and both
/// array kinds expose different APIs in `toml_edit`; this unifies them.
enum Node<'a> {
    Table(&'a mut Table),
    Inline(&'a mut InlineTable),
    Tables(&'a mut ArrayOfTables),
    Array(&'a mut Array),
}

impl<'a> Node<'a> {
    fn child(self, seg: &Seg) -> Result<Node<'a>, String> {
        let missing = || format!("missing {seg:?}");
        match (self, seg) {
            (Node::Table(t), Seg::Key(k)) => Node::item(t.get_mut(k).ok_or_else(missing)?),
            (Node::Inline(t), Seg::Key(k)) => Node::value(t.get_mut(k).ok_or_else(missing)?),
            (Node::Tables(a), Seg::Index(i)) => Ok(Node::Table(a.get_mut(*i).ok_or_else(missing)?)),
            (Node::Array(a), Seg::Index(i)) => Node::value(a.get_mut(*i).ok_or_else(missing)?),
            _ => Err(format!("path does not match document at {seg:?}")),
        }
    }

    fn item(item: &'a mut Item) -> Result<Node<'a>, String> {
        match item {
            Item::Table(t) => Ok(Node::Table(t)),
            Item::ArrayOfTables(a) => Ok(Node::Tables(a)),
            Item::Value(v) => Node::value(v),
            Item::None => Err("missing item".into()),
        }
    }

    fn value(value: &'a mut toml_edit::Value) -> Result<Node<'a>, String> {
        match value {
            toml_edit::Value::InlineTable(t) => Ok(Node::Inline(t)),
            toml_edit::Value::Array(a) => Ok(Node::Array(a)),
            _ => Err("expected a table or an array".into()),
        }
    }

    fn apply(self, op: &Op, last: &Seg) -> Result<(), String> {
        match (self, op, last) {
            (Node::Table(t), Op::Set(_, v), Seg::Key(k)) => set_key(t, k, v, true),
            (Node::Inline(t), Op::Set(_, v), Seg::Key(k)) => set_key(t, k, v, false),
            (Node::Table(t), Op::Remove(_), Seg::Key(k)) => {
                t.remove(k).map(drop).ok_or_else(|| "missing key".into())
            }
            (Node::Inline(t), Op::Remove(_), Seg::Key(k)) => {
                t.remove(k).map(drop).ok_or_else(|| "missing key".into())
            }
            (Node::Array(a), Op::Set(_, v), Seg::Index(i)) => {
                let mut new = inline(v)?;
                *new.decor_mut() = a.get(*i).ok_or("index out of range")?.decor().clone();
                a.replace_formatted(*i, new);
                Ok(())
            }
            (Node::Array(a), Op::Remove(_), Seg::Index(i)) => {
                bounded(a.len(), *i).map(|()| drop(a.remove(*i)))
            }
            (Node::Array(a), Op::Insert(_, v), Seg::Index(i)) => {
                a.insert(*i, inline(v)?);
                Ok(())
            }
            (Node::Tables(a), Op::Set(_, v), Seg::Index(i)) => {
                bounded(a.len(), *i)?;
                a.replace(*i, standard(v)?);
                Ok(())
            }
            (Node::Tables(a), Op::Remove(_), Seg::Index(i)) => {
                bounded(a.len(), *i).map(|()| drop(a.remove(*i)))
            }
            (Node::Tables(a), Op::Insert(_, v), Seg::Index(i)) => {
                a.insert(*i, standard(v)?);
                Ok(())
            }
            _ => Err("op does not fit its target".into()),
        }
    }
}

fn bounded(len: usize, index: usize) -> Result<(), String> {
    (index < len)
        .then_some(())
        .ok_or_else(|| "index out of range".into())
}

/// Set a key. An existing key is overwritten in place so its own comments
/// (stored on the key) and a replaced scalar's inline comment both survive.
fn set_key(
    t: &mut dyn TableLike,
    key: &str,
    value: &Value,
    standard_tables: bool,
) -> Result<(), String> {
    let mut item = if standard_tables {
        item(value)?
    } else {
        Item::Value(inline(value)?)
    };
    match t.get_mut(key) {
        Some(slot) => {
            if let (Item::Value(old), Item::Value(new)) = (&*slot, &mut item) {
                *new.decor_mut() = old.decor().clone();
            }
            *slot = item;
        }
        None => drop(t.insert(key, item)),
    }
    Ok(())
}

/// A value placed under a standard table: non-empty objects become tables.
fn item(value: &Value) -> Result<Item, String> {
    match value {
        Value::Object(map) if !map.is_empty() => Ok(Item::Table(standard(value)?)),
        _ => Ok(Item::Value(inline(value)?)),
    }
}

fn standard(value: &Value) -> Result<Table, String> {
    let Value::Object(map) = value else {
        return Err("expected a table".into());
    };
    let mut table = Table::new();
    for (key, value) in map {
        table.insert(key, item(value)?);
    }
    // A table holding only sub-tables needs no header of its own.
    let only_tables = table.iter().all(|(_, item)| item.is_table());
    table.set_implicit(only_tables);
    Ok(table)
}

fn inline(value: &Value) -> Result<toml_edit::Value, String> {
    Ok(match value {
        Value::Null => return Err("TOML has no null".into()),
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.into(),
            (None, Some(f)) if n.is_f64() => f.into(),
            _ => return Err(format!("{n} does not fit a TOML integer")),
        },
        Value::String(s) => s.as_str().into(),
        Value::Array(items) => items
            .iter()
            .map(inline)
            .collect::<Result<Array, _>>()?
            .into(),
        Value::Object(map) => {
            let mut table = InlineTable::new();
            for (key, value) in map {
                table.insert(key, inline(value)?);
            }
            table.into()
        }
    })
}

/// A whole document, for files created from scratch.
pub fn emit(value: &Value) -> Result<String, String> {
    let table = standard(value)?;
    let mut doc = DocumentMut::new();
    for (key, item) in table.iter() {
        doc.insert(key, item.clone());
    }
    Ok(doc.to_string())
}

fn table(t: &dyn TableLike) -> Value {
    Value::Object(
        t.iter()
            .filter_map(|(k, item)| Some((k.to_owned(), from_item(item)?)))
            .collect::<Map<_, _>>(),
    )
}

fn from_item(item: &Item) -> Option<Value> {
    match item {
        Item::None => None,
        Item::Value(v) => Some(from_value(v)),
        Item::Table(t) => Some(table(t)),
        Item::ArrayOfTables(a) => Some(Value::Array(a.iter().map(|t| table(t)).collect())),
    }
}

fn from_value(value: &toml_edit::Value) -> Value {
    match value {
        toml_edit::Value::String(s) => Value::String(s.value().clone()),
        toml_edit::Value::Integer(i) => Value::Number((*i.value()).into()),
        toml_edit::Value::Float(f) => {
            Number::from_f64(*f.value()).map_or_else(|| Value::String(f.to_string()), Value::Number)
        }
        toml_edit::Value::Boolean(b) => Value::Bool(*b.value()),
        toml_edit::Value::Datetime(d) => Value::String(d.value().to_string()),
        toml_edit::Value::Array(a) => Value::Array(a.iter().map(from_value).collect()),
        toml_edit::Value::InlineTable(t) => table(t),
    }
}
