//! Pure caption formatting; never performs filesystem or process queries.
use crate::config::MetadataSettings;
use compi_protocol::metadata::{MetadataField, MetadataState, PaneMetadata};

fn status_suffix<T>(field: &MetadataField<T>) -> &'static str {
    match field.state {
        MetadataState::Available => "",
        MetadataState::Stale => " [stale]",
        MetadataState::Unavailable => " [unavailable]",
    }
}

pub fn caption_fields(metadata: &PaneMetadata, settings: &MetadataSettings) -> Vec<String> {
    let mut fields = Vec::new();
    if settings.directory
        && let Some(path) = metadata.directory.value.as_deref()
    {
        let directory = path
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .filter(|part| !part.is_empty())
            .unwrap_or(path);
        fields.push(directory.to_owned());
    }
    if settings.process
        && let Some(process) = &metadata.process.value
    {
        fields.push(process.name.clone());
    }
    if settings.git
        && let Some(git) = &metadata.git.value
    {
        fields.push(git.branch.as_deref().unwrap_or("detached").to_owned());
    }
    if settings.dimensions {
        fields.push(format!(
            "{}×{}",
            metadata.dimensions.cols, metadata.dimensions.rows
        ));
    }
    fields
}

pub fn aggregate_caption(
    manual: &str,
    fallback: &str,
    fields: &[String],
    pane_count: usize,
    status: Option<&str>,
) -> (String, Option<String>) {
    let manual = manual.trim();
    let title = if manual.is_empty() { fallback } else { manual };
    let mut primary = title.to_owned();
    for field in fields.iter().filter(|field| field.as_str() != title) {
        primary.push_str(" · ");
        primary.push_str(field);
    }
    let secondary = match (status, pane_count > 1) {
        (Some(status), true) => Some(format!("{status} · {pane_count} panes")),
        (Some(status), false) => Some(status.to_owned()),
        (None, true) => Some(format!("{pane_count} panes")),
        (None, false) => None,
    };
    (primary, secondary)
}

/// Dirty state stays separate from truncating names. Freshness belongs in pane details.
pub fn caption_status(
    metadata: &PaneMetadata,
    settings: &MetadataSettings,
) -> Option<&'static str> {
    (settings.git
        && metadata.state == MetadataState::Available
        && metadata.git.state == MetadataState::Available
        && metadata.git.value.as_ref().is_some_and(|git| git.changed))
    .then_some("*")
}

pub fn details(metadata: &PaneMetadata) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "Environment: {:?}{} · {}{}",
        metadata.environment.kind,
        metadata
            .environment
            .distribution
            .as_ref()
            .map_or(String::new(), |name| format!(" ({name})")),
        metadata
            .environment
            .hostname
            .value
            .as_deref()
            .unwrap_or("hostname"),
        status_suffix(&metadata.environment.hostname)
    ));
    lines.push(format!(
        "Directory: {}{}",
        metadata
            .directory
            .value
            .as_deref()
            .unwrap_or("not reported"),
        status_suffix(&metadata.directory)
    ));
    lines.push(format!(
        "Process: {}{}",
        metadata.process.value.as_ref().map_or_else(
            || "not available".into(),
            |process| format!(
                "{} (PID {}, group {})",
                process.name, process.pid, process.process_group
            )
        ),
        status_suffix(&metadata.process)
    ));
    lines.push(format!(
        "Git: {}{}",
        metadata.git.value.as_ref().map_or_else(
            || {
                if metadata.git.state == MetadataState::Available {
                    "not a worktree".into()
                } else {
                    "not available".into()
                }
            },
            |git| format!(
                "{} · {}",
                git.branch.as_deref().unwrap_or("detached HEAD"),
                if git.changed { "changes" } else { "clean" }
            ),
        ),
        status_suffix(&metadata.git)
    ));
    lines.push(format!(
        "Dimensions: {} columns × {} rows",
        metadata.dimensions.cols, metadata.dimensions.rows
    ));
    for (label, reason) in [
        ("Directory", &metadata.directory.reason),
        ("Process", &metadata.process.reason),
        ("Git", &metadata.git.reason),
    ] {
        if let Some(reason) = reason {
            lines.push(format!("{label}: {reason}"));
        }
    }
    lines.join("\n")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_fields_do_not_replace_observed_names_and_dirty_requires_fresh_git() {
        use compi_protocol::metadata::{
            EnvironmentIdentity, EnvironmentKind, GitMetadata, PaneDimensions,
        };
        let mut metadata = PaneMetadata {
            surface_id: "surface".into(),
            process_lifetime_id: "lifetime".into(),
            state: MetadataState::Available,
            environment: EnvironmentIdentity {
                kind: EnvironmentKind::Unix,
                hostname: MetadataField::unavailable("not collected"),
                distribution: None,
            },
            collected_at_ms: Some(1),
            title: String::new(),
            shell_executable: None,
            directory: MetadataField::available("/work/project".to_owned()),
            process: MetadataField::unavailable("terminal ended"),
            git: MetadataField::unavailable("not collected"),
            dimensions: PaneDimensions { cols: 80, rows: 24 },
        };
        let settings = MetadataSettings {
            directory: true,
            process: true,
            git: true,
            dimensions: false,
        };
        assert_eq!(caption_fields(&metadata, &settings), vec!["project"]);
        assert_eq!(caption_status(&metadata, &settings), None);
        metadata.directory.mark_stale("terminal ended");
        assert_eq!(caption_fields(&metadata, &settings), vec!["project"]);
        metadata.git = MetadataField::available(GitMetadata {
            branch: Some("main".into()),
            commit: None,
            changed: true,
        });
        assert_eq!(
            caption_fields(&metadata, &settings),
            vec!["project", "main"]
        );
        assert_eq!(caption_status(&metadata, &settings), Some("*"));
        metadata.git.mark_stale("terminal ended");
        assert_eq!(caption_status(&metadata, &settings), None);
    }

    #[test]
    fn manual_label_precedes_automatic_fields_and_status_survives_path_truncation() {
        let (primary, badge) = aggregate_caption(
            "  My project  ",
            "bash",
            &["long-directory-name".into(), "main *".into()],
            3,
            Some("stale"),
        );
        assert_eq!(primary, "My project · long-directory-name · main *");
        assert_eq!(badge.as_deref(), Some("stale · 3 panes"));
        assert_eq!(
            aggregate_caption("", "editor", &[], 1, None),
            ("editor".into(), None)
        );
    }
}
