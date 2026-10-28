//! Structural diff between two format-neutral values.
//!
//! Merge logic runs on `serde_json::Value`, which is format neutral and keeps
//! key order. To write the result without losing comments, harnesscfg turns
//! the before/after pair into a short list of [`Op`]s and lets each format
//! backend replay them on its lossless syntax tree. Untouched nodes, and the
//! comments around them, are never rewritten.

use serde_json::{Map, Value};

/// One step from a document root to a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seg {
    Key(String),
    Index(usize),
}

pub type Path = Vec<Seg>;

/// An edit on a document. Ops are emitted in replay order: every path is valid
/// against the document as left by the ops before it.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// Replace the value at `path`, or add it when `path` ends in a new key.
    Set(Path, Value),
    /// Delete an object key or an array element.
    Remove(Path),
    /// Insert an array element; `path` ends in its new index.
    Insert(Path, Value),
}

/// Ops that turn `old` into `new`. Both roots must have the same kind; callers
/// rewrite the whole document otherwise.
pub fn diff(old: &Value, new: &Value) -> Vec<Op> {
    let mut ops = Vec::new();
    walk(&mut Vec::new(), old, new, &mut ops);
    ops
}

fn walk(path: &mut Path, old: &Value, new: &Value, ops: &mut Vec<Op>) {
    if old == new {
        return;
    }
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => objects(path, old, new, ops),
        (Value::Array(old), Value::Array(new)) => arrays(path, old, new, ops),
        _ => ops.push(Op::Set(path.clone(), new.clone())),
    }
}

fn objects(path: &mut Path, old: &Map<String, Value>, new: &Map<String, Value>, ops: &mut Vec<Op>) {
    for key in old.keys().filter(|key| !new.contains_key(*key)) {
        ops.push(Op::Remove(child(path, Seg::Key(key.clone()))));
    }
    for (key, value) in new {
        path.push(Seg::Key(key.clone()));
        match old.get(key) {
            Some(previous) => walk(path, previous, value, ops),
            None => ops.push(Op::Set(path.clone(), value.clone())),
        }
        path.pop();
    }
}

/// Arrays of objects keyed by a unique `id` (model catalogs, patch lists) are
/// matched by id, so editing one model leaves its neighbours and their
/// comments alone. Other arrays are matched by position.
fn arrays(path: &mut Path, old: &[Value], new: &[Value], ops: &mut Vec<Op>) {
    if let (Some(old_ids), Some(new_ids)) = (ids(old), ids(new)) {
        let kept: Vec<&str> = old_ids
            .iter()
            .copied()
            .filter(|id| new_ids.contains(id))
            .collect();
        let order: Vec<&str> = new_ids
            .iter()
            .copied()
            .filter(|id| kept.contains(id))
            .collect();
        if kept == order {
            return keyed(path, old, new, &old_ids, &new_ids, ops);
        }
    }
    let shared = old.len().min(new.len());
    for index in 0..shared {
        path.push(Seg::Index(index));
        walk(path, &old[index], &new[index], ops);
        path.pop();
    }
    for index in (shared..old.len()).rev() {
        ops.push(Op::Remove(child(path, Seg::Index(index))));
    }
    for (index, value) in new.iter().enumerate().skip(shared) {
        ops.push(Op::Insert(child(path, Seg::Index(index)), value.clone()));
    }
}

fn keyed(
    path: &mut Path,
    old: &[Value],
    new: &[Value],
    old_ids: &[&str],
    new_ids: &[&str],
    ops: &mut Vec<Op>,
) {
    // Remove from the end so earlier indices stay valid.
    for (index, id) in old_ids.iter().enumerate().rev() {
        if !new_ids.contains(id) {
            ops.push(Op::Remove(child(path, Seg::Index(index))));
        }
    }
    // Survivors now sit in `new` order, interleaved with nothing; walk them in
    // place, then insert additions at their final indices from left to right.
    let survivors: Vec<&Value> = old_ids
        .iter()
        .zip(old)
        .filter(|(id, _)| new_ids.contains(id))
        .map(|(_, v)| v)
        .collect();
    let mut position = 0;
    for (index, (id, value)) in new_ids.iter().zip(new).enumerate() {
        if old_ids.contains(id) {
            path.push(Seg::Index(index));
            walk(path, survivors[position], value, ops);
            path.pop();
            position += 1;
        } else {
            ops.push(Op::Insert(child(path, Seg::Index(index)), value.clone()));
        }
    }
}

/// Element ids when every element is an object with a distinct string `id`.
fn ids(items: &[Value]) -> Option<Vec<&str>> {
    let ids: Vec<&str> = items
        .iter()
        .map(|item| item.get("id")?.as_str())
        .collect::<Option<_>>()?;
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    (sorted.len() == ids.len()).then_some(ids)
}

fn child(path: &Path, seg: Seg) -> Path {
    let mut path = path.clone();
    path.push(seg);
    path
}

/// Replay `ops` on a plain value. The format backends must agree with this
/// reference implementation; tests compare them against it.
pub fn apply(root: &mut Value, ops: &[Op]) {
    for op in ops {
        let (path, last) = match op {
            Op::Set(path, _) | Op::Remove(path) | Op::Insert(path, _) => {
                path.split_at(path.len() - 1)
            }
        };
        let mut node = &mut *root;
        for seg in path {
            node = match seg {
                Seg::Key(key) => &mut node[key.as_str()],
                Seg::Index(index) => &mut node[*index],
            };
        }
        match (op, &last[0], node) {
            (Op::Set(_, value), Seg::Key(key), Value::Object(map)) => {
                map.insert(key.clone(), value.clone());
            }
            (Op::Set(_, value), Seg::Index(index), Value::Array(items)) => {
                items[*index] = value.clone()
            }
            (Op::Remove(_), Seg::Key(key), Value::Object(map)) => {
                map.shift_remove(key);
            }
            (Op::Remove(_), Seg::Index(index), Value::Array(items)) => {
                items.remove(*index);
            }
            (Op::Insert(_, value), Seg::Index(index), Value::Array(items)) => {
                items.insert(*index, value.clone())
            }
            _ => unreachable!("diff produced an op that does not fit its target"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn roundtrip(old: Value, new: Value) -> Vec<Op> {
        let ops = diff(&old, &new);
        let mut replay = old;
        apply(&mut replay, &ops);
        assert_eq!(replay, new);
        ops
    }

    #[test]
    fn equal_values_need_no_ops() {
        assert!(roundtrip(json!({"a": [1, {"b": 2}]}), json!({"a": [1, {"b": 2}]})).is_empty());
    }

    #[test]
    fn keyed_arrays_touch_only_changed_records() {
        let ops = roundtrip(
            json!({"models": [{"id": "keep", "cw": 1}, {"id": "retire"}, {"id": "same", "x": 0}]}),
            json!({"models": [{"id": "keep", "cw": 2}, {"id": "same", "x": 0}, {"id": "added"}]}),
        );
        assert_eq!(
            ops,
            vec![
                Op::Remove(vec![Seg::Key("models".into()), Seg::Index(1)]),
                Op::Set(
                    vec![
                        Seg::Key("models".into()),
                        Seg::Index(0),
                        Seg::Key("cw".into())
                    ],
                    json!(2)
                ),
                Op::Insert(
                    vec![Seg::Key("models".into()), Seg::Index(2)],
                    json!({"id": "added"})
                ),
            ]
        );
    }

    #[test]
    fn additions_before_survivors_land_at_their_final_index() {
        roundtrip(
            json!([{"id": "b"}]),
            json!([{"id": "a"}, {"id": "b", "v": 1}, {"id": "c"}]),
        );
    }

    #[test]
    fn reordered_or_unkeyed_arrays_fall_back_to_positions() {
        roundtrip(
            json!([{"id": "a"}, {"id": "b"}]),
            json!([{"id": "b"}, {"id": "a"}]),
        );
        roundtrip(json!(["x", "y", "z"]), json!(["y"]));
        roundtrip(json!(["x"]), json!(["x", "y", "z"]));
    }

    #[test]
    fn kind_changes_replace_the_node() {
        roundtrip(json!({"a": {"b": 1}}), json!({"a": [1]}));
        roundtrip(json!({"a": 1, "b": 2}), json!({"b": 3, "c": 4}));
    }
}
