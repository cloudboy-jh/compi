//! File-backed Markdown notebooks. Loading and indexing never write to the notebook.
use crate::{DaemonClient, content_path};
use compi_protocol::{ContentEnvironment, FileContent};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::Serialize;
use std::collections::HashSet;
use std::ops::Range;
use std::time::{Duration, Instant};

const MAX_DIRECTORIES: usize = 2_048;
const MAX_ENTRIES: usize = 32_768;
const MAX_NOTES: usize = 4_096;
const MAX_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const MAX_LOAD_TIME: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct Note {
    pub path: String,
    pub text: String,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct Notebook {
    pub root: String,
    pub windows: bool,
    pub notes: Vec<Note>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub line: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Byte offset of the space, `x` or `X` inside a parsed task's brackets.
    pub marker: usize,
    pub checked: bool,
    pub line: u32,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiLink {
    pub target: String,
    pub label: String,
    pub line: u32,
    /// The complete `[[target|label]]` in the original UTF-8 source.
    pub range: Range<usize>,
}

/// Shared parser extensions for the editor's renderer and source-aware operations.
pub fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH
}

pub fn is_markdown(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, extension)| {
        extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
    })
}

fn windows_path(path: &str) -> bool {
    path.starts_with("\\\\")
        || path.starts_with("//")
        || (path.as_bytes().get(1) == Some(&b':')
            && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic))
}

fn key(path: &str, windows: bool) -> String {
    if windows {
        path.replace('/', "\\").to_lowercase()
    } else {
        path.to_owned()
    }
}

fn within(root: &str, path: &str, windows: bool) -> bool {
    let root = key(root, windows);
    let path = key(path, windows);
    path == root
        || path
            .strip_prefix(root.trim_end_matches(if windows { '\\' } else { '/' }))
            .is_some_and(|rest| rest.starts_with(if windows { '\\' } else { '/' }))
}

fn filename(path: &str) -> &str {
    if windows_path(path) {
        path.rsplit(['/', '\\']).next().unwrap_or(path)
    } else {
        path.rsplit('/').next().unwrap_or(path)
    }
}

fn stem(path: &str) -> &str {
    let file = filename(path);
    if is_markdown(file) {
        file.rsplit_once('.').map_or(file, |(stem, _)| stem)
    } else {
        file
    }
}

/// Recursively read a bounded snapshot via the existing environment-aware transport.
/// An unreadable root is an error; unreadable descendants and exhausted limits are
/// warnings accompanying the notes that were successfully read. Directory listings
/// use `DirEntry::file_type`, not followed metadata, so directory symlinks are not
/// traversed. A visited set and a depth bound also protect against repeated entries.
pub fn load(
    client: &mut DaemonClient,
    environment: ContentEnvironment,
    root: &str,
) -> crate::Result<Notebook> {
    let windows = windows_path(root);
    let root = content_path::resolve(None, root, windows)?;
    let first = client.list_content_directory(environment.clone(), root.clone())?;
    let mut notebook = Notebook {
        root: root.clone(),
        windows,
        notes: Vec::new(),
        warnings: Vec::new(),
    };
    let mut pending = vec![(root, 0usize, Some(first))];
    let mut seen = HashSet::new();
    let mut entries_seen = 0usize;
    let mut text_bytes = 0usize;
    let started = Instant::now();
    'scan: while let Some((directory, depth, listed)) = pending.pop() {
        if started.elapsed() >= MAX_LOAD_TIME {
            notebook
                .warnings
                .push("Notebook loading stopped after 15 seconds; results are partial.".into());
            break;
        }
        if seen.len() >= MAX_DIRECTORIES {
            notebook.warnings.push(format!(
                "Notebook directory limit ({MAX_DIRECTORIES}) reached; results are partial."
            ));
            break;
        }
        if !seen.insert(key(&directory, windows)) {
            continue;
        }
        let mut entries = match listed {
            Some(entries) => entries,
            None => match client.list_content_directory(environment.clone(), directory.clone()) {
                Ok(entries) => entries,
                Err(error) => {
                    notebook
                        .warnings
                        .push(format!("Cannot list {directory}: {error}"));
                    continue;
                }
            },
        };
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in entries {
            entries_seen += 1;
            if entries_seen > MAX_ENTRIES || started.elapsed() >= MAX_LOAD_TIME {
                notebook.warnings.push(format!("Notebook scan limit ({MAX_ENTRIES} entries or 15 seconds) reached; results are partial."));
                break 'scan;
            }
            // A listing name must be one component in this environment's syntax.
            if entry.name.is_empty()
                || matches!(entry.name.as_str(), "." | "..")
                || entry.name.contains(['/', '\0'])
                || windows && entry.name.contains(['\\', ':'])
            {
                notebook.warnings.push(format!(
                    "Ignored invalid entry name in {directory}: {:?}",
                    entry.name
                ));
                continue;
            }
            let path = content_path::resolve(Some(&directory), &entry.name, windows)?;
            if entry.is_directory {
                if [".git", "target", "node_modules"].iter().any(|skip| {
                    if windows {
                        entry.name.eq_ignore_ascii_case(skip)
                    } else {
                        entry.name == *skip
                    }
                }) {
                    continue;
                }
                if depth >= MAX_DEPTH {
                    notebook.warnings.push(format!(
                        "Notebook depth limit ({MAX_DEPTH}) reached at {path}; results are partial."
                    ));
                } else if pending.len() < MAX_DIRECTORIES {
                    pending.push((path, depth + 1, None));
                } else {
                    notebook.warnings.push(format!("Notebook directory queue limit ({MAX_DIRECTORIES}) reached at {path}; results are partial."));
                }
                continue;
            }
            if !is_markdown(&entry.name) {
                continue;
            }
            if notebook.notes.len() >= MAX_NOTES {
                notebook.warnings.push(format!(
                    "Notebook note limit ({MAX_NOTES}) reached; results are partial."
                ));
                break 'scan;
            }
            match client.read_content_file(environment.clone(), path.clone()) {
                Ok(FileContent::Text { text, .. }) => {
                    if text_bytes + text.len() > MAX_TEXT_BYTES {
                        notebook.warnings.push(
                            "Notebook text limit (32 MiB) reached; results are partial.".into(),
                        );
                        break 'scan;
                    }
                    text_bytes += text.len();
                    notebook.notes.push(Note {
                        title: title(&text, &path),
                        path,
                        text,
                    });
                }
                Ok(FileContent::TooLarge { len }) => notebook.warnings.push(format!(
                    "Skipped {path}: {len} bytes exceeds the file text limit."
                )),
                Ok(FileContent::Binary { .. }) => notebook
                    .warnings
                    .push(format!("Skipped {path}: not UTF-8 text.")),
                Err(error) => notebook
                    .warnings
                    .push(format!("Cannot read {path}: {error}")),
            }
        }
    }
    notebook.notes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(notebook)
}

/// First Markdown heading, stripped of formatting, or the filename without extension.
pub fn title(source: &str, path: &str) -> String {
    let mut heading = None;
    for event in Parser::new_ext(source, markdown_options()) {
        match event {
            Event::Start(Tag::Heading { .. }) => heading = Some(String::new()),
            Event::End(TagEnd::Heading(_)) => {
                if let Some(text) = heading.take().filter(|text| !text.trim().is_empty()) {
                    return text.trim().to_owned();
                }
            }
            Event::Text(text) | Event::Code(text) if heading.is_some() => {
                heading.as_mut().unwrap().push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak if heading.is_some() => {
                heading.as_mut().unwrap().push(' ');
            }
            _ => {}
        }
    }
    stem(path).to_owned()
}

impl Notebook {
    /// Case-insensitive substring search of text, filenames and heading titles.
    /// A filename/title-only match points to line 1; content matches retain each line.
    pub fn search(&self, query: &str) -> Vec<SearchHit> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        for note in &self.notes {
            let before = hits.len();
            for (index, line) in note.text.lines().enumerate() {
                if line.to_lowercase().contains(&query) {
                    hits.push(SearchHit {
                        path: note.path.clone(),
                        line: (index + 1) as u32,
                        text: line.to_owned(),
                    });
                }
            }
            if hits.len() == before
                && (filename(&note.path).to_lowercase().contains(&query)
                    || note.title.to_lowercase().contains(&query))
            {
                hits.push(SearchHit {
                    path: note.path.clone(),
                    line: 1,
                    text: note.title.clone(),
                });
            }
        }
        hits.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
        hits
    }

    /// Resolve links in source-relative, then root-relative order. Only bare names
    /// fall back to a unique basename or heading title. Windows paths ignore case;
    /// Unix paths and titles are case-sensitive. Ambiguity is never guessed away.
    pub fn resolve(&self, from: &str, target: &str) -> Result<String, String> {
        let target = target
            .split_once('|')
            .map_or(target, |(target, _)| target)
            .trim();
        let target = target
            .split_once('#')
            .map_or(target, |(target, _)| target)
            .trim();
        if target.contains('\0') {
            return Err("Note links cannot contain NUL.".into());
        }
        let from = if from.is_empty() {
            None
        } else {
            Some(content_path::resolve(Some(&self.root), from, self.windows)?)
        };
        if target.is_empty() {
            let from = from.ok_or("A heading-only link needs an existing source note.")?;
            return self
                .exact(&from)?
                .ok_or_else(|| "A heading-only link needs an existing source note.".into());
        }
        let parent = from
            .as_deref()
            .map(|path| content_path::resolve(Some(path), "..", self.windows))
            .transpose()?;
        for base in [parent.as_deref(), Some(self.root.as_str())]
            .into_iter()
            .flatten()
        {
            let path = content_path::resolve(Some(base), target, self.windows)?;
            if !within(&self.root, &path, self.windows) {
                return Err(format!("Note link leaves the notebook: {target}"));
            }
            if let Some(found) = self.exact(&path)? {
                return Ok(found);
            }
        }
        if !target.contains('/') && (!self.windows || !target.contains(['\\', ':'])) {
            let wanted = key(stem(target), self.windows);
            let matches = self
                .notes
                .iter()
                .filter(|note| key(stem(&note.path), self.windows) == wanted);
            if let Some(path) = unique(matches, target)? {
                return Ok(path);
            }
            let wanted = key(target, self.windows);
            let matches = self
                .notes
                .iter()
                .filter(|note| key(&note.title, self.windows) == wanted);
            if let Some(path) = unique(matches, target)? {
                return Ok(path);
            }
        }
        Err(format!("Note not found: {target}"))
    }

    fn exact(&self, path: &str) -> Result<Option<String>, String> {
        let path_key = key(path, self.windows);
        let matches = self.notes.iter().filter(|note| {
            let actual = key(&note.path, self.windows);
            actual == path_key
                || !is_markdown(path)
                    && is_markdown(&actual)
                    && actual
                        .rsplit_once('.')
                        .is_some_and(|(stem, _)| stem == path_key)
        });
        unique(matches, path)
    }

    /// One hit per referring source line; unresolved/ambiguous links are not backlinks.
    pub fn backlinks(&self, path: &str) -> Vec<SearchHit> {
        let Ok(target) = self.resolve("", path) else {
            return Vec::new();
        };
        let mut hits = Vec::new();
        for note in &self.notes {
            let mut last_line = 0;
            let mut source_lines = note.text.lines();
            for link in wiki_links(&note.text) {
                if self
                    .resolve(&note.path, &link.target)
                    .is_ok_and(|found| found == target)
                    && link.line != last_line
                {
                    let text = source_lines
                        .nth((link.line - last_line - 1) as usize)
                        .unwrap_or("")
                        .to_owned();
                    last_line = link.line;
                    hits.push(SearchHit {
                        path: note.path.clone(),
                        line: link.line,
                        text,
                    });
                }
            }
        }
        hits.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
        hits
    }
}

fn unique<'a>(
    matches: impl IntoIterator<Item = &'a Note>,
    target: &str,
) -> Result<Option<String>, String> {
    let mut matches = matches.into_iter();
    let Some(first) = matches.next() else {
        return Ok(None);
    };
    let Some(second) = matches.next() else {
        return Ok(Some(first.path.clone()));
    };
    let mut paths = format!("{}, {}", first.path, second.path);
    for note in matches {
        paths.push_str(", ");
        paths.push_str(&note.path);
    }
    Err(format!("Ambiguous note link {target}: {paths}"))
}

fn advance_line(source: &str, previous: &mut usize, line: &mut u32, offset: usize) {
    *line += source.as_bytes()[*previous..offset]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count() as u32;
    *previous = offset;
}

/// Extract wiki links from original source, excluding real inline and block code.
/// Links stay on one line and may use an optional display label after `|`.
pub fn wiki_links(source: &str) -> Vec<WikiLink> {
    let mut code = Vec::new();
    for (event, range) in Parser::new_ext(source, markdown_options()).into_offset_iter() {
        if matches!(event, Event::Code(_) | Event::Start(Tag::CodeBlock(_))) {
            code.push(range);
        }
    }
    let mut links = Vec::new();
    let mut offset = 0;
    let mut previous = 0;
    let mut line = 1;
    let mut code_index = 0;
    while let Some(start) = source[offset..].find("[[").map(|index| offset + index) {
        offset = start + 2;
        let escapes = source.as_bytes()[..start]
            .iter()
            .rev()
            .take_while(|byte| **byte == b'\\')
            .count();
        if escapes % 2 != 0 {
            continue;
        }
        let line_end = source[offset..]
            .find(['\r', '\n'])
            .map_or(source.len(), |index| offset + index);
        let Some(end) = source[offset..line_end]
            .find("]]")
            .map(|index| offset + index)
        else {
            offset = line_end;
            continue;
        };
        let range = start..end + 2;
        offset = range.end;
        while code
            .get(code_index)
            .is_some_and(|blocked| blocked.end <= start)
        {
            code_index += 1;
        }
        if code
            .get(code_index)
            .is_some_and(|blocked| blocked.start < range.end)
        {
            continue;
        }
        let inside = &source[start + 2..end];
        let (target, label) = inside.split_once('|').unwrap_or((inside, inside));
        let target = target.trim();
        if !target.is_empty() {
            advance_line(source, &mut previous, &mut line, start);
            links.push(WikiLink {
                target: target.to_owned(),
                label: label.trim().to_owned(),
                line,
                range,
            });
        }
    }
    links
}

/// Task markers come exclusively from Markdown parser events, not text matching.
pub fn tasks(source: &str) -> Vec<Task> {
    let mut result = Vec::new();
    let mut previous = 0;
    let mut line = 1;
    for (event, range) in Parser::new_ext(source, markdown_options()).into_offset_iter() {
        let Event::TaskListMarker(checked) = event else {
            continue;
        };
        // The parser's source range includes the brackets; preserve every other byte.
        let Some(marker) = source[range.clone()]
            .as_bytes()
            .windows(3)
            .position(|bytes| {
                bytes[0] == b'[' && matches!(bytes[1], b' ' | b'x' | b'X') && bytes[2] == b']'
            })
            .map(|index| range.start + index + 1)
        else {
            continue;
        };
        let label = source[marker + 2..]
            .split(['\r', '\n'])
            .next()
            .unwrap_or("")
            .trim()
            .to_owned();
        advance_line(source, &mut previous, &mut line, marker);
        result.push(Task {
            marker,
            checked,
            line,
            label,
        });
    }
    result
}

pub fn toggle_task(source: &str, marker: usize) -> Result<String, String> {
    let task = tasks(source)
        .into_iter()
        .find(|task| task.marker == marker)
        .ok_or_else(|| {
            "The source no longer has a Markdown task at this byte offset.".to_owned()
        })?;
    let mut text = source.to_owned();
    text.replace_range(marker..marker + 1, if task.checked { " " } else { "x" });
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notebook(root: &str, files: &[(&str, &str)]) -> Notebook {
        let windows = windows_path(root);
        Notebook {
            root: root.into(),
            windows,
            warnings: Vec::new(),
            notes: files
                .iter()
                .map(|(path, text)| {
                    let path = content_path::resolve(Some(root), path, windows).unwrap();
                    Note {
                        title: title(text, &path),
                        path,
                        text: (*text).into(),
                    }
                })
                .collect(),
        }
    }

    #[test]
    fn task_offsets_preserve_unicode_crlf_and_exclude_code() {
        let source = "é 🦀\r\n- [ ] café\r\n> - [X] nested\r\n\r\n`- [ ] inline`\r\n\r\n```md\r\n- [ ] fenced\r\n```\r\n\r\n    - [ ] indented\r\n\r\n[ ] not a task\r\n";
        let tasks = tasks(source);
        assert_eq!(tasks.len(), 2);
        assert_eq!((tasks[0].line, tasks[0].label.as_str()), (2, "café"));
        assert_eq!((tasks[1].line, tasks[1].checked), (3, true));
        let changed = toggle_task(source, tasks[0].marker).unwrap();
        assert_eq!(changed, source.replacen("- [ ] café", "- [x] café", 1));
        assert_eq!(
            toggle_task(source, tasks[1].marker).unwrap(),
            source.replacen("[X]", "[ ]", 1)
        );
        assert!(toggle_task(source, source.find("[ ] inline").unwrap() + 1).is_err());
        assert!(toggle_task(source, 1).is_err());
    }

    #[test]
    fn links_exclude_markdown_code_and_preserve_source_ranges() {
        let source = "é [[note#Heading|label]]\n`[[inline]]`\n\n```\n[[fenced]]\n```\n\n    [[indented]]\n\n\\[[escaped]] and [[real]]";
        let links = wiki_links(source);
        assert_eq!(
            links
                .iter()
                .map(|link| link.target.as_str())
                .collect::<Vec<_>>(),
            ["note#Heading", "real"]
        );
        assert_eq!(&source[links[0].range.clone()], "[[note#Heading|label]]");
        assert_eq!(links[0].label, "label");
    }

    #[test]
    fn resolution_uses_relative_then_root_and_rejects_ambiguity() {
        let notebook = notebook(
            "/notes",
            &[
                ("a/source.md", ""),
                ("a/topic.MD", "# Local"),
                ("topic.md", "# Root"),
                ("b/shared.md", "# Shared"),
                ("c/shared.md", "# Shared"),
                ("d/title-file.md", "# A title"),
            ],
        );
        assert_eq!(
            notebook
                .resolve("/notes/a/source.md", "topic#Part")
                .unwrap(),
            "/notes/a/topic.MD"
        );
        assert_eq!(
            notebook
                .resolve("/notes/a/source.md", "../topic.md")
                .unwrap(),
            "/notes/topic.md"
        );
        assert_eq!(
            notebook.resolve("/notes/a/source.md", "#Part").unwrap(),
            "/notes/a/source.md"
        );
        assert_eq!(
            notebook.resolve("", "A title").unwrap(),
            "/notes/d/title-file.md"
        );
        assert!(
            notebook
                .resolve("", "shared")
                .unwrap_err()
                .contains("Ambiguous")
        );
        assert!(notebook.resolve("", "missing").is_err());
        assert!(notebook.resolve("", "../topic").is_err());
        assert!(notebook.resolve("", "TOPIC").is_err());
        let windows = notebook_windows();
        assert_eq!(
            windows
                .resolve(r"c:\NOTES\source.md", "TOPIC#Part")
                .unwrap(),
            r"C:\Notes\Topic.md"
        );
        let windows_root = self::notebook(r"C:\", &[("source.md", ""), ("Topic.md", "")]);
        assert_eq!(
            windows_root.resolve(r"C:\source.md", "topic").unwrap(),
            r"C:\Topic.md"
        );
    }

    fn notebook_windows() -> Notebook {
        notebook(r"C:\Notes", &[("source.md", ""), ("Topic.md", "# Topic")])
    }

    #[test]
    fn search_and_backlinks_use_real_targets_and_deduplicate_lines() {
        let notebook = notebook(
            "/notes",
            &[
                ("target.md", "# A heading\nNeedle here\nneedle again"),
                (
                    "source.md",
                    "[[target|label]] [[target#Part]]\n`[[target]]`\n[[absent]]\n",
                ),
                ("Needle-file.md", "unrelated"),
            ],
        );
        let hits = notebook.search("NEEDLE");
        assert_eq!(
            hits.iter()
                .map(|hit| (hit.path.as_str(), hit.line))
                .collect::<Vec<_>>(),
            [
                ("/notes/Needle-file.md", 1),
                ("/notes/target.md", 2),
                ("/notes/target.md", 3)
            ]
        );
        let hits = notebook.backlinks("target.md");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            (hits[0].path.as_str(), hits[0].line),
            ("/notes/source.md", 1)
        );
    }
}
