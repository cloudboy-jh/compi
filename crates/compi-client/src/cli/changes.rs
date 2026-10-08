//! `compi changes`: the running version and the notes What's New shows for it. Reads
//! only the embedded notes, so it works with no Compi running.
use super::Output;
use crate::release_notes::{self, ReleaseNotes};
use serde_json::json;

const RUNNING: &str = env!("CARGO_PKG_VERSION");

pub(super) fn run() -> Output {
    report(release_notes::current())
}

/// The first summary bullet is the headline, as in `compi update --check`; the other
/// summary bullets follow, then each section under its title.
fn report(notes: Option<&ReleaseNotes>) -> Output {
    let Some((summary, highlights, sections)) = notes.and_then(|notes| {
        let (summary, highlights) = notes.summary.split_first()?;
        Some((summary, highlights, &notes.details))
    }) else {
        return Output::Report {
            text: format!("Compi {RUNNING} · no release notes are recorded for this version\n"),
            value: json!({"version": RUNNING, "summary": null, "highlights": [], "sections": []}),
        };
    };
    let mut text = format!("Compi {RUNNING} · {summary}\n");
    for note in highlights {
        text.push_str(&format!("  {note}\n"));
    }
    for section in sections {
        text.push_str(&format!("\n{}\n", section.title));
        for note in &section.bullets {
            text.push_str(&format!("  {note}\n"));
        }
    }
    Output::Report {
        text,
        value: json!({
            "version": RUNNING,
            "summary": summary,
            "highlights": highlights,
            "sections": sections
                .iter()
                .map(|section| json!({"title": section.title, "notes": section.bullets}))
                .collect::<Vec<_>>(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_every_whats_new_note_of_the_running_version_by_section() {
        let notes = release_notes::current().expect("shipped notes cover the running version");
        let Output::Report { text, value } = run() else {
            panic!("changes reports text and a value");
        };
        assert_eq!(value["version"], RUNNING);
        assert_eq!(value["summary"], notes.summary[0]);
        assert_eq!(value["highlights"], json!(notes.summary[1..]));
        let sections = value["sections"].as_array().unwrap();
        assert_eq!(sections.len(), notes.details.len());
        for (json, section) in sections.iter().zip(&notes.details) {
            assert_eq!(json["title"], section.title);
            assert_eq!(json["notes"], json!(section.bullets));
        }

        let mut expected = vec![format!("Compi {RUNNING} · {}", notes.summary[0])];
        expected.extend(notes.summary[1..].iter().map(|note| format!("  {note}")));
        for section in &notes.details {
            expected.extend([String::new(), section.title.clone()]);
            expected.extend(section.bullets.iter().map(|note| format!("  {note}")));
        }
        assert_eq!(text.lines().collect::<Vec<_>>(), expected);
    }
}
