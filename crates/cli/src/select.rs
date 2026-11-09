//! Refs that stay stable across snapshots, and the selectors that find elements.
//!
//! A selector is one or more parts joined by `>>`; each part searches inside the element the
//! previous part found. Parts: `@e12`, `#id`, `text=Save`, `ctx=Overlay`, `src=window.rs:4321`,
//! `button "Next"`, `button`, `nth=2`, or plain text.

use crate::fail::{Fail, fail};
use crate::instance::state_root;
use crate::snapshot::{self, Entry, Frame, Key, Options, Snapshot};
use anyhow::Result;
use herdr_ctrl_protocol::RawTree;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

const ROLES: &[&str] = &[
    "button",
    "textbox",
    "textinput",
    "scroll",
    "draggable",
    "droptarget",
    "group",
    "overlay",
    "text",
    "checkbox",
    "switch",
    "link",
    "list",
    "listitem",
    "tab",
    "menuitem",
    "dialog",
];

#[derive(Default, Serialize, Deserialize)]
struct RefTable {
    pid: u32,
    window: u64,
    last: usize,
    entries: Vec<RefRecord>,
}

#[derive(Serialize, Deserialize)]
struct RefRecord {
    reference: String,
    key: Key,
}

pub struct Refs {
    path: PathBuf,
    table: RefTable,
}

impl Refs {
    pub fn open(target: &str, agent: &str, pid: u32, window: u64) -> Refs {
        let path = state_root().join("refs").join(format!("{target}.{agent}.json"));
        let mut table: RefTable = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        if table.pid != pid || table.window != window {
            table = RefTable {
                pid,
                window,
                ..Default::default()
            };
        }
        Refs { path, table }
    }

    pub fn assign(&mut self, snapshot: &mut Snapshot) -> Result<()> {
        for entry in &mut snapshot.entries {
            entry.reference = match self.table.entries.iter().find(|r| r.key == entry.key) {
                Some(record) => record.reference.clone(),
                None => {
                    self.table.last += 1;
                    let reference = format!("e{}", self.table.last);
                    self.table.entries.push(RefRecord {
                        reference: reference.clone(),
                        key: entry.key.clone(),
                    });
                    reference
                }
            };
        }
        let excess = self.table.entries.len().saturating_sub(5000);
        self.table.entries.drain(..excess);
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, serde_json::to_string(&self.table)?)?;
        Ok(())
    }

    fn key(&self, reference: &str) -> Option<&Key> {
        self.table.entries.iter().find(|r| r.reference == reference).map(|r| &r.key)
    }
}

pub fn snapshot(tree: &RawTree, refs: &mut Refs, options: &Options) -> Result<Snapshot> {
    let mut snapshot = snapshot::build(tree, options);
    refs.assign(&mut snapshot)?;
    Ok(snapshot)
}

enum Part {
    Ref(String),
    Id(String),
    Text(String),
    Context(String),
    Source(String),
    Role(String, Option<String>),
    Nth(usize),
}

fn tokens(part: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in part.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// One `>>` step: every atom in it must hold, as in `textbox ctx=Input` or `button "Next"`.
fn parse_part(part: &str) -> Result<Vec<Part>> {
    let part = part.trim();
    if let Some(text) = part.strip_prefix("text=") {
        return Ok(vec![Part::Text(unquote(text))]);
    }
    let mut atoms = Vec::new();
    let mut role: Option<String> = None;
    let mut free: Vec<String> = Vec::new();
    for (index, token) in tokens(part).into_iter().enumerate() {
        if let Some(reference) = token.strip_prefix('@') {
            atoms.push(Part::Ref(reference.to_string()));
        } else if let Some(id) = token.strip_prefix('#').or_else(|| token.strip_prefix("id=")) {
            atoms.push(Part::Id(id.to_string()));
        } else if let Some(context) = token.strip_prefix("ctx=") {
            atoms.push(Part::Context(context.to_string()));
        } else if let Some(source) = token.strip_prefix("src=") {
            atoms.push(Part::Source(source.to_string()));
        } else if let Some(n) = token.strip_prefix("nth=") {
            let n: usize = n.parse().map_err(|_| fail("usage", format!("{token:?}: nth needs a number from 1")))?;
            atoms.push(Part::Nth(n.max(1)));
        } else if index == 0 && ROLES.contains(&token.as_str()) {
            role = Some(token);
        } else {
            free.push(token);
        }
    }
    let name = (!free.is_empty()).then(|| free.join(" "));
    match (role, name) {
        (Some(role), name) => atoms.insert(0, Part::Role(role, name)),
        (None, Some(name)) => atoms.push(Part::Text(name)),
        (None, None) => {}
    }
    if atoms.is_empty() {
        return Err(fail("usage", "empty selector step"));
    }
    Ok(atoms)
}

fn unquote(text: &str) -> String {
    let text = text.trim();
    if text.len() >= 2 && (text.starts_with('"') && text.ends_with('"') || text.starts_with('\'') && text.ends_with('\'')) {
        text[1..text.len() - 1].to_string()
    } else {
        text.to_string()
    }
}

fn matches_text(entries: &[Entry], text: &str) -> Vec<Entry> {
    let exact: Vec<Entry> = entries.iter().filter(|e| e.name == text).cloned().collect();
    if !exact.is_empty() {
        return exact;
    }
    let lower = text.to_lowercase();
    entries
        .iter()
        .filter(|e| e.name.to_lowercase().contains(&lower))
        .cloned()
        .collect()
}

/// Every visible element the selector names, in tree order.
pub fn find_all(tree: &RawTree, refs: &mut Refs, selector: &str) -> Result<Vec<Entry>> {
    let frame = Frame::new(tree);
    let all = snapshot(tree, refs, &Options::default())?;
    let parts = selector
        .split(">>")
        .map(parse_part)
        .collect::<Result<Vec<_>>>()?;
    let mut scope: Option<usize> = None;
    let mut found: Vec<Entry> = Vec::new();
    for (index, atoms) in parts.iter().enumerate() {
        let only_nth = atoms.iter().all(|a| matches!(a, Part::Nth(_)));
        found = match (index, only_nth) {
            (_, true) => found.clone(),
            (0, _) => all.entries.clone(),
            _ => all
                .entries
                .iter()
                .filter(|e| scope.is_some_and(|s| e.node != s && frame.is_ancestor(s, e.node)))
                .cloned()
                .collect(),
        };
        for atom in atoms {
            found = apply(tree, &frame, refs, atom, found)?;
        }
        if found.is_empty() {
            return Ok(found);
        }
        scope = Some(found[0].node);
        let next_is_nth = parts
            .get(index + 1)
            .is_some_and(|next| next.iter().all(|a| matches!(a, Part::Nth(_))));
        if found.len() > 1 && index + 1 < parts.len() && !next_is_nth {
            return Err(ambiguous(selector, &found));
        }
    }
    Ok(prefer_elements(&frame, found))
}

fn apply(tree: &RawTree, frame: &Frame, refs: &Refs, atom: &Part, pool: Vec<Entry>) -> Result<Vec<Entry>> {
    Ok(match atom {
        Part::Ref(reference) => {
            let key = refs.key(reference).ok_or_else(|| {
                Fail::new("stale_ref", format!("@{reference} is not a ref of this window"))
                    .hint("take a new snapshot; refs reset when the window restarts")
            })?;
            match pool.iter().find(|e| &e.key == key).cloned() {
                Some(entry) => vec![entry],
                None if frame.find(key).is_some() => {
                    return Err(Fail::new("not_visible", format!("@{reference} exists but is not visible"))
                        .hint("scroll it into view or close what covers it")
                        .into());
                }
                None => {
                    return Err(Fail::new("stale_ref", format!("@{reference} is gone from the window"))
                        .hint("take a new snapshot")
                        .into());
                }
            }
        }
        Part::Id(id) => pool
            .into_iter()
            .filter(|e| tree.nodes[e.node].id.as_deref() == Some(id.as_str()) && !e.key.type_name.ends_with("#text"))
            .collect(),
        Part::Text(text) => matches_text(&pool, text),
        Part::Context(context) => pool
            .into_iter()
            .filter(|e| e.context.as_deref() == Some(context.as_str()))
            .collect(),
        Part::Source(source) => pool
            .into_iter()
            .filter(|e| e.source.as_deref().is_some_and(|s| s.contains(source.as_str())))
            .collect(),
        Part::Role(role, name) => {
            let with_role: Vec<Entry> = pool.into_iter().filter(|e| &e.role == role).collect();
            match name {
                Some(name) => matches_text(&with_role, name),
                None => with_role,
            }
        }
        Part::Nth(n) => pool.get(n - 1).cloned().into_iter().collect(),
    })
}

/// Drops a text line when the element that holds it matched too.
fn prefer_elements(frame: &Frame, found: Vec<Entry>) -> Vec<Entry> {
    let holders: HashSet<usize> = found.iter().filter(|e| e.role != "text").map(|e| e.node).collect();
    found
        .into_iter()
        .filter(|e| {
            e.role != "text" || !holders.iter().any(|&h| h == e.node || frame.is_ancestor(h, e.node))
        })
        .collect()
}

fn ambiguous(selector: &str, found: &[Entry]) -> anyhow::Error {
    let lines: Vec<String> = found.iter().take(8).map(describe).collect();
    Fail::new("ambiguous", format!("{selector:?} matches {} elements:\n  {}", found.len(), lines.join("\n  ")))
        .hint("use a ref from the list, add `>> nth=N`, or narrow it with `parent >> child`")
        .into()
}

pub fn describe(entry: &Entry) -> String {
    let mut line = format!("@{} {}", entry.reference, entry.role);
    if !entry.name.is_empty() {
        let name: String = entry.name.chars().take(60).collect();
        line.push_str(&format!(" {name:?}"));
    }
    if let Some(context) = &entry.context {
        line.push_str(&format!(" ctx={context}"));
    }
    line
}

/// The one element a selector names.
pub fn find_one(tree: &RawTree, refs: &mut Refs, selector: &str) -> Result<Entry> {
    let found = find_all(tree, refs, selector)?;
    match found.len() {
        0 => Err(Fail::new("not_found", format!("nothing visible matches {selector:?}"))
            .hint("run `herdr-ctrl snapshot` to see what is on screen")
            .into()),
        1 => Ok(found.into_iter().next().unwrap_or_else(|| unreachable!())),
        _ => Err(ambiguous(selector, &found)),
    }
}

/// The point to press on an element, refused when something else would receive it.
pub fn aim(tree: &RawTree, entry: &Entry, at: Option<(f32, f32)>) -> Result<(f32, f32)> {
    if entry.visible.is_empty() {
        return Err(Fail::new("not_visible", format!("{} is not visible", describe(entry)))
            .hint("scroll it into view or close what covers it")
            .into());
    }
    let (x, y) = match at {
        Some((fx, fy)) => entry.visible.at(fx, fy),
        None => entry.visible.center(),
    };
    let frame = Frame::new(tree);
    if let Some(hit) = frame.hit(x, y)
        && hit != entry.node
        && !frame.is_ancestor(entry.node, hit)
        && !frame.is_ancestor(hit, entry.node)
    {
        let node = &tree.nodes[hit];
        let mut blocker = format!("{} {:?}", frame.role(hit), frame.name(hit));
        if let Some(source) = &node.source {
            blocker.push_str(&format!(" from {source}"));
        }
        return Err(Fail::new("covered", format!("{} is covered at ({x:.0}, {y:.0}) by {blocker}", describe(entry)))
            .hint("close the covering element first, or aim elsewhere with --at")
            .into());
    }
    Ok((x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn describe_parts(part: &str) -> Vec<String> {
        parse_part(part)
            .unwrap()
            .into_iter()
            .map(|atom| match atom {
                Part::Ref(r) => format!("ref {r}"),
                Part::Id(i) => format!("id {i}"),
                Part::Text(t) => format!("text {t}"),
                Part::Context(c) => format!("ctx {c}"),
                Part::Source(s) => format!("src {s}"),
                Part::Role(r, n) => format!("role {r} {n:?}"),
                Part::Nth(n) => format!("nth {n}"),
            })
            .collect()
    }

    #[test]
    fn a_step_combines_role_name_and_attributes() {
        assert_eq!(describe_parts(r#"button "Split right" nth=2"#), ["role button Some(\"Split right\")", "nth 2"]);
        assert_eq!(describe_parts("textbox ctx=Input"), ["role textbox None", "ctx Input"]);
        assert_eq!(describe_parts("#space-new"), ["id space-new"]);
        assert_eq!(describe_parts("Split right"), ["text Split right"]);
        assert_eq!(describe_parts("text=button ctx=x"), ["text button ctx=x"]);
        assert_eq!(describe_parts("@e12"), ["ref e12"]);
    }
}
