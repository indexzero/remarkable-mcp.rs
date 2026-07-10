//! In-memory view over a flat list of [`Item`]s.
//!
//! The cloud returns items flat (each with a parent UUID); everything hierarchical
//! — full paths, folder listings, the ASCII tree — is reconstructed here. This is
//! pure, deterministic logic and is the most heavily unit-tested part of the crate.
//!
//! Ported from lanej's `buildPathMap` / `BuildTree` / `ToASCII` / `ListFolder` /
//! `Search`, with `did_you_mean` fuzzy matching added from the SamMorrowDrums /
//! wavyrai recover-don't-fail playbook.

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::model::{Item, ItemType};

/// A hierarchical view over an immutable snapshot of library items.
pub struct Library {
    items: Vec<Item>,
    by_id: HashMap<String, usize>,
}

impl Library {
    /// Build a library from a flat item list.
    pub fn new(items: Vec<Item>) -> Self {
        let by_id = items
            .iter()
            .enumerate()
            .map(|(i, it)| (it.id.clone(), i))
            .collect();
        Library { items, by_id }
    }

    /// All items, in their original order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Number of items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// `true` if the library has no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn get(&self, id: &str) -> Option<&Item> {
        self.by_id.get(id).map(|&i| &self.items[i])
    }

    /// Map every item id to its absolute `/`-delimited path.
    ///
    /// Memoized and cycle-safe: a parent chain that loops (or points at a missing
    /// parent) terminates at the root rather than recursing forever.
    pub fn path_map(&self) -> HashMap<String, String> {
        let mut paths: HashMap<String, String> = HashMap::with_capacity(self.items.len());
        for item in &self.items {
            self.resolve_path(item, &mut paths, 0);
        }
        paths
    }

    fn resolve_path(
        &self,
        item: &Item,
        paths: &mut HashMap<String, String>,
        depth: usize,
    ) -> String {
        if let Some(p) = paths.get(&item.id) {
            return p.clone();
        }
        // Guard against pathological parent cycles.
        let path = if depth > 256 {
            format!("/{}", item.name)
        } else {
            match &item.parent {
                None => format!("/{}", item.name),
                Some(parent_id) => match self.get(parent_id) {
                    Some(parent) => {
                        let parent_path = self.resolve_path(parent, paths, depth + 1);
                        format!("{}/{}", parent_path, item.name)
                    }
                    None => format!("/{}", item.name),
                },
            }
        };
        paths.insert(item.id.clone(), path.clone());
        path
    }

    /// Normalize a user-supplied path: trim, ensure a leading `/`, drop trailing `/`.
    fn normalize_path(path: &str) -> String {
        let p = path.trim();
        let p = p.strip_suffix('/').unwrap_or(p);
        if p.is_empty() {
            "/".to_string()
        } else if let Some(stripped) = p.strip_prefix('/') {
            format!("/{stripped}")
        } else {
            format!("/{p}")
        }
    }

    /// Look up an item by its absolute path.
    pub fn by_path(&self, path: &str) -> Option<&Item> {
        let target = Self::normalize_path(path);
        let paths = self.path_map();
        paths
            .iter()
            .find(|(_, p)| **p == target)
            .and_then(|(id, _)| self.get(id))
    }

    /// Look up an item by UUID first, then by path (lanej's dual addressing).
    pub fn by_path_or_id(&self, needle: &str) -> Option<&Item> {
        if uuid::Uuid::parse_str(needle.trim()).is_ok() {
            if let Some(item) = self.get(needle.trim()) {
                return Some(item);
            }
        }
        self.by_path(needle)
    }

    /// List the immediate (or, when `recursive`, all nested) contents of a folder.
    /// `"/"` lists the library root.
    pub fn list_folder(&self, path: &str, recursive: bool) -> Result<Vec<&Item>> {
        let target = Self::normalize_path(path);
        let paths = self.path_map();

        let folder_id: Option<String> = if target == "/" {
            None
        } else {
            let id = paths
                .iter()
                .find(|(id, p)| {
                    **p == target && self.get(id).map(|i| i.is_folder()).unwrap_or(false)
                })
                .map(|(id, _)| id.clone());
            match id {
                Some(id) => Some(id),
                None => return Err(Error::NotFound(format!("folder not found: {target}"))),
            }
        };

        let mut result: Vec<&Item> = Vec::new();
        for item in &self.items {
            if recursive {
                let item_path = &paths[&item.id];
                let prefix = if target == "/" { "/" } else { target.as_str() };
                let under = if target == "/" {
                    true
                } else {
                    item_path.starts_with(prefix) && *item_path != target
                };
                if under && *item_path != target {
                    result.push(item);
                }
            } else if item.parent.as_deref() == folder_id.as_deref() {
                result.push(item);
            }
        }
        result.sort_by_key(|a| name_key(a));
        Ok(result)
    }

    /// Case-insensitive substring search over item names, optionally filtered by kind.
    /// Returns `(item, absolute_path)` pairs sorted by path.
    pub fn search(&self, query: &str, filter: Option<ItemType>) -> Vec<(&Item, String)> {
        let q = query.to_lowercase();
        let paths = self.path_map();
        let mut out: Vec<(&Item, String)> = self
            .items
            .iter()
            .filter(|it| filter.map(|f| it.kind == f).unwrap_or(true))
            .filter(|it| it.name.to_lowercase().contains(&q))
            .map(|it| (it, paths.get(&it.id).cloned().unwrap_or_default()))
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1));
        out
    }

    /// The `limit` most recently modified documents (folders excluded), newest first.
    pub fn recent(&self, limit: usize) -> Vec<&Item> {
        let mut docs: Vec<&Item> = self.items.iter().filter(|it| !it.is_folder()).collect();
        docs.sort_by_key(|d| std::cmp::Reverse(d.last_modified));
        docs.truncate(limit);
        docs
    }

    /// Suggest up to `n` item names closest to `query`, for "did you mean…?" hints.
    /// Prefers substring matches, then falls back to smallest edit distance.
    pub fn did_you_mean(&self, query: &str, n: usize) -> Vec<String> {
        let q = query.to_lowercase();
        let mut substr: Vec<&str> = self
            .items
            .iter()
            .filter(|it| it.name.to_lowercase().contains(&q))
            .map(|it| it.name.as_str())
            .collect();
        substr.sort();
        substr.dedup();
        if !substr.is_empty() {
            substr.truncate(n);
            return substr.into_iter().map(String::from).collect();
        }
        let mut scored: Vec<(usize, &str)> = self
            .items
            .iter()
            .map(|it| (levenshtein(&q, &it.name.to_lowercase()), it.name.as_str()))
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
        scored
            .into_iter()
            .take(n)
            .map(|(_, name)| name.to_string())
            .collect()
    }

    /// Build the sorted root nodes of the folder/document tree (trashed items
    /// already excluded upstream by [`Item::from_metadata`]).
    pub fn build_tree(&self) -> Vec<TreeNode<'_>> {
        let mut children_of: HashMap<Option<&str>, Vec<&Item>> = HashMap::new();
        for item in &self.items {
            children_of
                .entry(item.parent.as_deref())
                .or_default()
                .push(item);
        }
        // A parent id that doesn't resolve to a known folder is treated as root.
        let known: std::collections::HashSet<&str> =
            self.items.iter().map(|i| i.id.as_str()).collect();

        let mut roots: Vec<&Item> = self
            .items
            .iter()
            .filter(|it| match &it.parent {
                None => true,
                Some(p) => !known.contains(p.as_str()),
            })
            .collect();
        roots.sort_by_key(|a| name_key(a));
        roots
            .into_iter()
            .map(|it| TreeNode::build(it, &children_of))
            .collect()
    }

    /// Render the library (or a subtree rooted at `start_path`) as an ASCII tree.
    /// `max_depth` of 0 means unlimited; depth 1 collapses each folder's contents
    /// to a `… (N items)` summary (lanej's depth behavior).
    pub fn render_tree(&self, start_path: Option<&str>, max_depth: usize) -> Result<String> {
        let roots = self.build_tree();
        let render_roots: Vec<TreeNode> = match start_path {
            None => roots,
            Some(path) => {
                let target = Self::normalize_path(path);
                if target == "/" {
                    roots
                } else {
                    let node = find_node(&roots, &target, "");
                    match node {
                        Some(n) => vec![n.clone_shallow()],
                        None => return Err(Error::NotFound(format!("path not found: {target}"))),
                    }
                }
            }
        };
        let mut out = String::new();
        let last = render_roots.len().saturating_sub(1);
        for (i, node) in render_roots.iter().enumerate() {
            node.render("", i == last, 1, max_depth, &mut out);
        }
        Ok(out)
    }
}

/// A node in the document tree.
#[derive(Clone)]
pub struct TreeNode<'a> {
    /// The item at this node.
    pub item: &'a Item,
    /// Child nodes, sorted by name.
    pub children: Vec<TreeNode<'a>>,
}

impl<'a> TreeNode<'a> {
    fn build(item: &'a Item, children_of: &HashMap<Option<&'a str>, Vec<&'a Item>>) -> Self {
        let mut kids: Vec<&Item> = children_of
            .get(&Some(item.id.as_str()))
            .cloned()
            .unwrap_or_default();
        kids.sort_by_key(|a| name_key(a));
        TreeNode {
            item,
            children: kids
                .into_iter()
                .map(|k| TreeNode::build(k, children_of))
                .collect(),
        }
    }

    /// A clone that keeps children (used when re-rooting a render at a subtree).
    fn clone_shallow(&self) -> TreeNode<'a> {
        self.clone()
    }

    /// Recursively render this node. `depth` is 1-based; `max_depth == 0` is unlimited.
    fn render(
        &self,
        prefix: &str,
        is_last: bool,
        depth: usize,
        max_depth: usize,
        out: &mut String,
    ) {
        let connector = if is_last { "└── " } else { "├── " };
        out.push_str(prefix);
        out.push_str(connector);
        out.push_str(self.item.kind.icon());
        out.push(' ');
        out.push_str(&self.item.name);
        out.push('\n');

        let child_prefix = format!("{prefix}{}", if is_last { "    " } else { "│   " });

        if max_depth != 0 && depth >= max_depth {
            if !self.children.is_empty() {
                out.push_str(&child_prefix);
                out.push_str(&format!("└── … ({} items)\n", self.children.len()));
            }
            return;
        }

        let last = self.children.len().saturating_sub(1);
        for (i, child) in self.children.iter().enumerate() {
            child.render(&child_prefix, i == last, depth + 1, max_depth, out);
        }
    }
}

/// Find a node whose absolute path equals `target`.
fn find_node<'a>(nodes: &[TreeNode<'a>], target: &str, prefix: &str) -> Option<TreeNode<'a>> {
    for node in nodes {
        let path = format!("{prefix}/{}", node.item.name);
        if path == target {
            return Some(node.clone());
        }
        if let Some(found) = find_node(&node.children, target, &path) {
            return Some(found);
        }
    }
    None
}

/// Case-insensitive sort key for stable, human-friendly ordering.
fn name_key(item: &Item) -> String {
    item.name.to_lowercase()
}

/// Classic iterative Levenshtein edit distance (two-row), for `did_you_mean`.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    const D1: &str = "11111111-1111-4111-8111-111111111111"; // Inbox (root doc)
    const F1: &str = "22222222-2222-4222-8222-222222222222"; // Work (root folder)
    const D2: &str = "33333333-3333-4333-8333-333333333333"; // Work/Q3 Planning
    const F2: &str = "44444444-4444-4444-8444-444444444444"; // Work/Archive
    const D3: &str = "55555555-5555-4555-8555-555555555555"; // Work/Archive/Old Notes
    const F3: &str = "66666666-6666-4666-8666-666666666666"; // Personal (root folder)

    fn item(id: &str, name: &str, kind: ItemType, parent: Option<&str>, modified_ms: i64) -> Item {
        Item {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            parent: parent.map(String::from),
            version: 1,
            pinned: false,
            last_modified: chrono::DateTime::from_timestamp_millis(modified_ms),
        }
    }

    fn fixture() -> Library {
        Library::new(vec![
            item(D1, "Inbox", ItemType::Document, None, 100),
            item(F1, "Work", ItemType::Folder, None, 0),
            item(D2, "Q3 Planning", ItemType::Document, Some(F1), 500),
            item(F2, "Archive", ItemType::Folder, Some(F1), 0),
            item(D3, "Old Notes", ItemType::Document, Some(F2), 300),
            item(F3, "Personal", ItemType::Folder, None, 0),
        ])
    }

    #[test]
    fn path_map_resolves_nested_paths() {
        let lib = fixture();
        let paths = lib.path_map();
        assert_eq!(paths[D1], "/Inbox");
        assert_eq!(paths[F1], "/Work");
        assert_eq!(paths[D2], "/Work/Q3 Planning");
        assert_eq!(paths[F2], "/Work/Archive");
        assert_eq!(paths[D3], "/Work/Archive/Old Notes");
        assert_eq!(paths[F3], "/Personal");
    }

    #[test]
    fn by_path_and_dual_addressing() {
        let lib = fixture();
        assert_eq!(lib.by_path("/Work/Q3 Planning").unwrap().id, D2);
        // Path without a leading slash is normalized.
        assert_eq!(lib.by_path("Work/Archive").unwrap().id, F2);
        // Dual addressing: a UUID resolves directly.
        assert_eq!(lib.by_path_or_id(D3).unwrap().name, "Old Notes");
        // Unknown path resolves to nothing.
        assert!(lib.by_path("/Nope").is_none());
    }

    #[test]
    fn list_folder_immediate_children_sorted() {
        let lib = fixture();
        let root: Vec<&str> = lib
            .list_folder("/", false)
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(root, vec!["Inbox", "Personal", "Work"]);

        let work: Vec<&str> = lib
            .list_folder("/Work", false)
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(work, vec!["Archive", "Q3 Planning"]);
    }

    #[test]
    fn list_folder_recursive_includes_descendants() {
        let lib = fixture();
        let mut names: Vec<&str> = lib
            .list_folder("/Work", true)
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        names.sort();
        assert_eq!(names, vec!["Archive", "Old Notes", "Q3 Planning"]);
    }

    #[test]
    fn list_folder_unknown_errors() {
        let lib = fixture();
        assert!(matches!(
            lib.list_folder("/Ghost", false),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn search_filters_by_kind() {
        let lib = fixture();
        let all = lib.search("a", None); // matches Q3 Planning, Archive, Personal
        assert!(all.len() >= 3);
        let folders = lib.search("a", Some(ItemType::Folder));
        assert!(folders.iter().all(|(it, _)| it.is_folder()));
        let notes = lib.search("notes", None);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].0.id, D3);
        assert_eq!(notes[0].1, "/Work/Archive/Old Notes");
    }

    #[test]
    fn recent_sorts_documents_newest_first() {
        let lib = fixture();
        let recent: Vec<&str> = lib.recent(10).iter().map(|i| i.name.as_str()).collect();
        // Documents only, by modified desc: Q3(500) > Old Notes(300) > Inbox(100).
        assert_eq!(recent, vec!["Q3 Planning", "Old Notes", "Inbox"]);
    }

    #[test]
    fn did_you_mean_prefers_substring_then_edit_distance() {
        let lib = fixture();
        // Substring hit.
        assert_eq!(lib.did_you_mean("work", 3), vec!["Work"]);
        // No substring; nearest by edit distance is the planning doc.
        let near = lib.did_you_mean("Q3 Planing", 1);
        assert_eq!(near, vec!["Q3 Planning"]);
    }

    #[test]
    fn tree_renders_with_icons_and_nesting() {
        let lib = fixture();
        let tree = lib.render_tree(None, 0).unwrap();
        assert!(tree.contains("📁 Work"));
        assert!(tree.contains("📄 Q3 Planning"));
        assert!(tree.contains("📄 Old Notes"));
        // Inbox (root) appears before Personal and Work alphabetically.
        let inbox = tree.find("Inbox").unwrap();
        let work = tree.find("Work").unwrap();
        assert!(inbox < work);
    }

    #[test]
    fn tree_depth_collapses_children() {
        let lib = fixture();
        let shallow = lib.render_tree(None, 1).unwrap();
        // Depth 1 shows top-level nodes but collapses Work's contents.
        assert!(shallow.contains("📁 Work"));
        assert!(shallow.contains("items)"));
        assert!(!shallow.contains("Q3 Planning"));
    }

    #[test]
    fn tree_can_root_at_a_subpath() {
        let lib = fixture();
        let sub = lib.render_tree(Some("/Work"), 0).unwrap();
        assert!(sub.contains("📁 Work"));
        assert!(sub.contains("Archive"));
        assert!(!sub.contains("Personal"));
        assert!(matches!(
            lib.render_tree(Some("/Nope"), 0),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn path_resolution_is_cycle_safe() {
        // Two folders that point at each other must not infinite-loop.
        let lib = Library::new(vec![
            item(F1, "A", ItemType::Folder, Some(F2), 0),
            item(F2, "B", ItemType::Folder, Some(F1), 0),
        ]);
        let paths = lib.path_map(); // must terminate
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("same", "same"), 0);
    }
}
