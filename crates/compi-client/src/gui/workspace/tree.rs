use compi_protocol::{ContentEnvironment, PaneId, SurfaceId};

#[derive(Clone, Debug)]
pub(in crate::gui) struct Entry {
    pub name: String,
    pub directory: bool,
}

#[derive(Clone, Debug)]
pub(in crate::gui) struct Row {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub directory: bool,
    pub expanded: bool,
    pub loading: bool,
    pub has_next_sibling: bool,
}

pub(in crate::gui) struct FileTree {
    pub pane_id: PaneId,
    pub rows: Vec<Row>,
    pub search_results: Vec<Row>,
    pub selected: usize,
    pub query: String,
    pub searching: bool,
    pub pending_shell: bool,
    pub project_mode: bool,
    /// The terminal surface the daemon lists and searches folders through.
    pub surface_id: Option<SurfaceId>,
    /// Open file…: a chosen file opens in a content pane of this environment.
    pub opens_in: Option<ContentEnvironment>,
    pub error: Option<String>,
    /// Switch branch…: the picker lists a repository's branches instead of files.
    pub branches: Option<super::branches::BranchPicker>,
    pub request: u64,
    pub search_request: u64,
}

impl FileTree {
    pub fn new(pane_id: PaneId, path: String) -> Self {
        let name = path
            .rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .unwrap_or("/");
        Self {
            pane_id,
            rows: vec![Row {
                name: name.to_owned(),
                path,
                depth: 0,
                directory: true,
                expanded: true,
                loading: true,
                has_next_sibling: false,
            }],
            search_results: Vec::new(),
            selected: 0,
            query: String::new(),
            searching: false,
            pending_shell: false,
            project_mode: false,
            surface_id: None,
            opens_in: None,
            error: None,
            branches: None,
            request: 1,
            search_request: 0,
        }
    }

    pub fn selected(&self) -> Option<&Row> {
        self.display_rows().get(self.selected)
    }

    pub fn display_rows(&self) -> &[Row] {
        if self.project_mode {
            &self.search_results
        } else if !self.searching || self.query.is_empty() {
            &self.rows
        } else {
            &self.search_results
        }
    }

    pub fn replace_search_results(&mut self, paths: Vec<(String, bool)>) {
        self.search_results = paths
            .into_iter()
            .map(|(path, directory)| {
                let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
                Row {
                    path,
                    name,
                    depth: 0,
                    directory,
                    expanded: false,
                    loading: false,
                    has_next_sibling: false,
                }
            })
            .collect();
        self.selected = 0;
    }

    pub fn replace_children(&mut self, parent: &str, mut entries: Vec<Entry>) {
        let Some(index) = self.rows.iter().position(|row| row.path == parent) else {
            return;
        };
        self.rows[index].loading = false;
        if !self.rows[index].expanded {
            return;
        }
        let depth = self.rows[index].depth;
        let end = self.rows[index + 1..]
            .iter()
            .position(|row| row.depth <= depth)
            .map_or(self.rows.len(), |offset| index + 1 + offset);
        let selected_path = self.rows.get(self.selected).map(|row| row.path.clone());
        self.rows.drain(index + 1..end);
        entries.retain(|entry| entry.name != "." && entry.name != "..");
        let child_count = entries.len();
        let children = entries.into_iter().enumerate().map(|(index, entry)| Row {
            path: join_path(parent, &entry.name),
            name: entry.name,
            depth: depth + 1,
            directory: entry.directory,
            expanded: false,
            loading: false,
            has_next_sibling: index + 1 < child_count,
        });
        self.rows.splice(index + 1..index + 1, children);
        self.selected = selected_path
            .and_then(|path| self.rows.iter().position(|row| row.path == path))
            .unwrap_or(index);
    }

    /// Returns the path that needs an asynchronous directory listing.
    pub fn toggle(&mut self, index: usize) -> Option<String> {
        let row = self.rows.get(index)?;
        if !row.directory || row.loading {
            return None;
        }
        let depth = row.depth;
        if row.expanded {
            let end = self.rows[index + 1..]
                .iter()
                .position(|child| child.depth <= depth)
                .map_or(self.rows.len(), |offset| index + 1 + offset);
            self.rows.drain(index + 1..end);
            self.rows[index].expanded = false;
            if self.selected > index && self.selected < end {
                self.selected = index;
            } else if self.selected >= end {
                self.selected -= end - index - 1;
            }
            None
        } else {
            self.rows[index].expanded = true;
            self.rows[index].loading = true;
            Some(self.rows[index].path.clone())
        }
    }

    pub fn move_selection(&mut self, step: isize) {
        let len = match &self.branches {
            Some(branches) => branches.matches.len(),
            None => self.display_rows().len(),
        };
        if len > 0 {
            self.selected = self.selected.saturating_add_signed(step).min(len - 1);
        }
    }
}

/// `name` inside `parent`, joined with the separator `parent` uses: native Windows
/// folders of a content pane use backslashes, every other path forward slashes.
pub(super) fn join_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        return format!("/{name}");
    }
    let separator = if parent.contains('\\') && !parent.contains('/') {
        '\\'
    } else {
        '/'
    };
    format!("{}{separator}{name}", parent.trim_end_matches(['/', '\\']))
}

/// Indices of the `rows` a filter shows: those whose name contains `filter`, ignoring
/// case, and the folders above them. An empty filter shows every row.
pub(super) fn filter_rows(rows: &[Row], filter: &str) -> Vec<usize> {
    if filter.is_empty() {
        return (0..rows.len()).collect();
    }
    let needle = filter.to_lowercase();
    let mut shown = vec![false; rows.len()];
    // The latest row at each depth above the current one: the folders holding it.
    let mut folders: Vec<usize> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        folders.truncate(row.depth);
        if contains_folded(&row.name, &needle) {
            shown[index] = true;
            // A shown folder's own folders are shown already.
            for &folder in folders.iter().rev() {
                if std::mem::replace(&mut shown[folder], true) {
                    break;
                }
            }
        }
        folders.push(index);
    }
    shown
        .iter()
        .enumerate()
        .filter_map(|(index, shown)| shown.then_some(index))
        .collect()
}

/// Whether `name` contains the already lowercase `needle`, ignoring case.
fn contains_folded(name: &str, needle: &str) -> bool {
    name.char_indices().any(|(start, _)| {
        let mut rest = name[start..].chars().flat_map(char::to_lowercase);
        needle.chars().all(|wanted| rest.next() == Some(wanted))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_collapse_retains_siblings_and_selection() {
        let mut tree = FileTree::new(PaneId::new("pane"), "/work".into());
        tree.replace_children(
            "/work",
            vec![
                Entry {
                    name: "src".into(),
                    directory: true,
                },
                Entry {
                    name: "README.md".into(),
                    directory: false,
                },
            ],
        );
        assert_eq!(tree.toggle(1).as_deref(), Some("/work/src"));
        tree.replace_children(
            "/work/src",
            vec![Entry {
                name: "main.rs".into(),
                directory: false,
            }],
        );
        assert_eq!(
            tree.rows
                .iter()
                .map(|row| (row.depth, row.has_next_sibling))
                .collect::<Vec<_>>(),
            vec![(0, false), (1, true), (2, false), (1, false)]
        );
        tree.selected = 3;
        tree.toggle(1);
        assert_eq!(
            tree.rows
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            vec!["work", "src", "README.md"]
        );
        assert_eq!(tree.selected, 2);
        assert_eq!(join_path("/", "file name"), "/file name");
    }

    #[test]
    fn search_results_navigate_paths_outside_expanded_tree() {
        let mut tree = FileTree::new(PaneId::new("pane"), "/work".into());
        tree.query = "bt".into();
        tree.searching = true;
        tree.replace_search_results(vec![
            ("/work/apps/beta.rs".into(), false),
            ("/work/other/beta.txt".into(), false),
        ]);
        tree.move_selection(1);
        assert_eq!(tree.selected().unwrap().path, "/work/other/beta.txt");
    }

    #[test]
    fn windows_folders_keep_backslashes() {
        let mut tree = FileTree::new(PaneId::new("pane"), r"C:\work\".into());
        assert_eq!(tree.rows[0].name, "work");
        tree.replace_children(
            r"C:\work\",
            vec![Entry {
                name: "main.rs".into(),
                directory: false,
            }],
        );
        assert_eq!(tree.rows[1].path, r"C:\work\main.rs");
        assert_eq!(join_path(r"C:\", "src"), r"C:\src");
        assert_eq!(join_path("/mnt/c/work/", "src"), "/mnt/c/work/src");
    }

    #[test]
    fn a_filter_keeps_matching_names_and_the_folders_above_them() {
        let entry = |name: &str, directory| Entry {
            name: name.into(),
            directory,
        };
        let mut tree = FileTree::new(PaneId::new("pane"), "/work".into());
        tree.replace_children(
            "/work",
            vec![
                entry("src", true),
                entry("docs", true),
                entry("README.md", false),
            ],
        );
        tree.toggle(1);
        tree.replace_children(
            "/work/src",
            vec![entry("gui", true), entry("Main.rs", false)],
        );
        tree.toggle(2);
        tree.replace_children("/work/src/gui", vec![entry("readme_view.rs", false)]);
        let names = |filter: &str| {
            filter_rows(&tree.rows, filter)
                .into_iter()
                .map(|index| tree.rows[index].name.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names("README"),
            ["work", "src", "gui", "readme_view.rs", "README.md"]
        );
        assert_eq!(names("main"), ["work", "src", "Main.rs"]);
        assert_eq!(names("docs"), ["work", "docs"]);
        assert!(names("absent").is_empty());
        assert_eq!(names("").len(), tree.rows.len());
    }
}
