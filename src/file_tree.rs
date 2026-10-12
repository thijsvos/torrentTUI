//! The add picker's folder tree: a torrent's files grouped by the folders in
//! their paths, so a whole season or a whole `extras/` folder is one row.
//!
//! Built once per picker from the preview's path components and never
//! changed, which is what lets a [`NodeId`] serve as a stable cursor anchor.
//! Files are referred to by their position in the picker's file list, the
//! same numbering the picker's selection uses.

use std::collections::{HashMap, HashSet};

use crate::add_picker::PickerSort;
use crate::types::PreviewFile;

pub type NodeId = usize;

#[derive(Debug)]
pub struct Node {
    pub name: String,
    /// 0 for top-level entries.
    pub depth: usize,
    pub parent: Option<NodeId>,
    /// Children for a folder, empty for a file.
    pub children: Vec<NodeId>,
    /// `Some(position)` for a file.
    pub file: Option<usize>,
    /// Positions of every file at or under this node, ascending — so the
    /// first one is where the node sits in torrent order.
    pub files: Vec<usize>,
    /// Total size of `files`.
    pub size: u64,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.file.is_none()
    }
}

/// One line of the flattened tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeRow {
    pub node: NodeId,
    pub depth: usize,
}

/// A folder's checkbox, from the files under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    All,
    Some,
    None,
}

#[derive(Debug, Default)]
pub struct FileTree {
    nodes: Vec<Node>,
    roots: Vec<NodeId>,
    /// The node of each file position.
    leaf: Vec<NodeId>,
}

impl FileTree {
    pub fn build(files: &[PreviewFile]) -> Self {
        let mut tree = Self::default();
        let mut dirs: HashMap<(Option<NodeId>, &str), NodeId> = HashMap::new();
        for (pos, file) in files.iter().enumerate() {
            let (leaf_name, folders) = match file.components.split_last() {
                Some((last, folders)) => (last.as_str(), folders),
                None => (file.path.as_str(), &[][..]),
            };
            let mut parent = None;
            for name in folders {
                parent = Some(match dirs.get(&(parent, name.as_str())) {
                    Some(&id) => id,
                    None => {
                        let id = tree.push(name, parent, None);
                        dirs.insert((parent, name.as_str()), id);
                        id
                    }
                });
            }
            let leaf = tree.push(leaf_name, parent, Some(pos));
            tree.leaf.push(leaf);
            // Every ancestor counts this file.
            let mut at = Some(leaf);
            while let Some(id) = at {
                tree.nodes[id].files.push(pos);
                tree.nodes[id].size += file.size;
                at = tree.nodes[id].parent;
            }
        }
        tree
    }

    fn push(&mut self, name: &str, parent: Option<NodeId>, file: Option<usize>) -> NodeId {
        let id = self.nodes.len();
        let depth = parent.map_or(0, |p| self.nodes[p].depth + 1);
        self.nodes.push(Node {
            name: name.to_string(),
            depth,
            parent,
            children: Vec::new(),
            file,
            files: Vec::new(),
            size: 0,
        });
        match parent {
            Some(p) => self.nodes[p].children.push(id),
            None => self.roots.push(id),
        }
        id
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    /// The node holding the file at `pos`.
    pub fn leaf(&self, pos: usize) -> Option<NodeId> {
        self.leaf.get(pos).copied()
    }

    /// Whether any file sits inside a folder — the tree is only worth
    /// opening in when there is something to fold.
    pub fn has_folders(&self) -> bool {
        self.nodes.iter().any(Node::is_dir)
    }

    /// The top-level folders, which the picker opens expanded.
    pub fn root_dirs(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.roots
            .iter()
            .copied()
            .filter(|&id| self.nodes[id].is_dir())
    }

    /// The tree as rows, folders before files at each level, each group in
    /// the picker's sort. With `matches`, only files in it are shown, with
    /// the folders leading to them — all open, whatever `expanded` says, so
    /// a match is never hidden inside a collapsed folder.
    pub fn rows(
        &self,
        expanded: &HashSet<NodeId>,
        matches: Option<&HashSet<usize>>,
        sort: PickerSort,
    ) -> Vec<TreeRow> {
        let mut out = Vec::new();
        self.push_rows(&self.roots, expanded, matches, sort, &mut out);
        out
    }

    fn push_rows(
        &self,
        ids: &[NodeId],
        expanded: &HashSet<NodeId>,
        matches: Option<&HashSet<usize>>,
        sort: PickerSort,
        out: &mut Vec<TreeRow>,
    ) {
        let mut ids: Vec<NodeId> = ids
            .iter()
            .copied()
            .filter(|&id| {
                matches.is_none_or(|m| self.nodes[id].files.iter().any(|f| m.contains(f)))
            })
            .collect();
        ids.sort_by(|&a, &b| {
            let (a, b) = (&self.nodes[a], &self.nodes[b]);
            b.is_dir().cmp(&a.is_dir()).then_with(|| match sort {
                PickerSort::TorrentOrder => a.files[0].cmp(&b.files[0]),
                PickerSort::Size => b.size.cmp(&a.size),
                PickerSort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            })
        });
        for id in ids {
            let node = &self.nodes[id];
            out.push(TreeRow {
                node: id,
                depth: node.depth,
            });
            if node.is_dir() && (matches.is_some() || expanded.contains(&id)) {
                self.push_rows(&node.children, expanded, matches, sort, out);
            }
        }
    }

    /// The files a key on this node acts on: all under it, or with a filter
    /// active, the ones the filter shows.
    pub fn files_in_view(&self, id: NodeId, matches: Option<&HashSet<usize>>) -> Vec<usize> {
        self.nodes[id]
            .files
            .iter()
            .copied()
            .filter(|f| matches.is_none_or(|m| m.contains(f)))
            .collect()
    }

    pub fn check(&self, id: NodeId, selected: &[bool], matches: Option<&HashSet<usize>>) -> Check {
        let files = self.files_in_view(id, matches);
        let on = files.iter().filter(|&&f| selected[f]).count();
        match on {
            0 => Check::None,
            n if n == files.len() => Check::All,
            _ => Check::Some,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> PreviewFile {
        PreviewFile {
            index: 0,
            components: path.split('/').map(str::to_string).collect(),
            path: path.to_string(),
            size,
        }
    }

    fn sample() -> (Vec<PreviewFile>, FileTree) {
        let files = vec![
            file("Season 1/e01.mkv", 10),
            file("Season 1/e02.mkv", 20),
            file("Season 2/extras/making-of.mkv", 5),
            file("Season 2/e01.mkv", 40),
            file("notes.txt", 1),
        ];
        let tree = FileTree::build(&files);
        (files, tree)
    }

    fn names(tree: &FileTree, rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), tree.node(r.node).name))
            .collect()
    }

    fn all_expanded(tree: &FileTree) -> HashSet<NodeId> {
        (0..tree.nodes.len())
            .filter(|&i| tree.node(i).is_dir())
            .collect()
    }

    #[test]
    fn builds_folders_from_path_components_with_sizes() {
        let (_, tree) = sample();
        assert!(tree.has_folders());
        let s2 = tree
            .root_dirs()
            .find(|&id| tree.node(id).name == "Season 2")
            .unwrap();
        assert_eq!(tree.node(s2).size, 45);
        assert_eq!(tree.node(s2).files, vec![2, 3]);
        let leaf = tree.leaf(2).unwrap();
        assert_eq!(tree.node(leaf).name, "making-of.mkv");
        assert_eq!(tree.node(leaf).depth, 2);
    }

    #[test]
    fn rows_put_folders_first_and_hide_collapsed_contents() {
        let (_, tree) = sample();
        let rows = tree.rows(&HashSet::new(), None, PickerSort::TorrentOrder);
        assert_eq!(
            names(&tree, &rows),
            vec!["Season 1", "Season 2", "notes.txt"]
        );

        let rows = tree.rows(&all_expanded(&tree), None, PickerSort::TorrentOrder);
        assert_eq!(
            names(&tree, &rows),
            vec![
                "Season 1",
                "  e01.mkv",
                "  e02.mkv",
                "Season 2",
                "  extras",
                "    making-of.mkv",
                "  e01.mkv",
                "notes.txt",
            ]
        );
    }

    #[test]
    fn rows_follow_the_sort_within_each_level() {
        let (_, tree) = sample();
        let rows = tree.rows(&all_expanded(&tree), None, PickerSort::Size);
        assert_eq!(names(&tree, &rows)[0], "Season 2", "45 beats 30");
        // Inside Season 2 the extras folder still leads, then e01 (40).
        assert_eq!(names(&tree, &rows)[1], "  extras");
        assert_eq!(names(&tree, &rows)[3], "  e01.mkv");
        assert_eq!(names(&tree, &rows)[4], "Season 1");
        let rows = tree.rows(&HashSet::new(), None, PickerSort::Name);
        assert_eq!(
            names(&tree, &rows),
            vec!["Season 1", "Season 2", "notes.txt"]
        );
    }

    #[test]
    fn a_filter_opens_the_way_to_every_match_and_nothing_else() {
        let (_, tree) = sample();
        let matches: HashSet<usize> = [2].into();
        // Nothing expanded, yet the match inside extras/ shows.
        let rows = tree.rows(&HashSet::new(), Some(&matches), PickerSort::TorrentOrder);
        assert_eq!(
            names(&tree, &rows),
            vec!["Season 2", "  extras", "    making-of.mkv"]
        );
    }

    #[test]
    fn folder_checkboxes_have_three_states() {
        let (_, tree) = sample();
        let s1 = tree.root_dirs().next().unwrap();
        assert_eq!(
            tree.check(s1, &[true, true, false, false, true], None),
            Check::All
        );
        assert_eq!(
            tree.check(s1, &[true, false, false, false, true], None),
            Check::Some
        );
        assert_eq!(
            tree.check(s1, &[false, false, true, true, true], None),
            Check::None
        );
    }

    #[test]
    fn under_a_filter_a_folder_speaks_for_the_files_shown() {
        let (_, tree) = sample();
        let s2 = tree.root_dirs().nth(1).unwrap();
        let matches: HashSet<usize> = [3].into();
        assert_eq!(tree.files_in_view(s2, Some(&matches)), vec![3]);
        // The hidden making-of is off, but the shown e01 is on: "all".
        assert_eq!(
            tree.check(s2, &[false, false, false, true, false], Some(&matches)),
            Check::All
        );
    }

    #[test]
    fn a_flat_torrent_has_no_folders() {
        let files = vec![file("a.iso", 1), file("b.iso", 2)];
        let tree = FileTree::build(&files);
        assert!(!tree.has_folders());
        let rows = tree.rows(&HashSet::new(), None, PickerSort::TorrentOrder);
        assert_eq!(rows.len(), 2);
    }
}
