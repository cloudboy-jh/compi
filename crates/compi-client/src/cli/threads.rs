//! `compi threads`: the chats the daemon keeps, open in a pane or ended, newest first;
//! and `compi threads delete ID`, which deletes an ended one.
use super::human::{self, Fit};
use super::{CliError, Options, Output};
use crate::DaemonClient;
use crate::chat::{age_label, now_ms, thread_status, thread_title};
use compi_protocol::chat::ThreadInfo;
use serde_json::json;

pub(super) fn run(client: &mut DaemonClient, options: &Options) -> Result<Output, CliError> {
    match options.args.get(1).map(String::as_str) {
        None => {
            let threads = client.list_threads()?;
            Ok(Output::Readable {
                text: table(&threads, now_ms(), human::width()),
                value: json!(threads),
            })
        }
        Some("delete") => {
            let chat = &options.args[2];
            client.delete_thread(chat)?;
            Ok(Output::Report {
                text: format!("Deleted thread {chat}\n"),
                value: json!({"chat": chat, "deleted": true}),
            })
        }
        Some(_) => unreachable!(),
    }
}

/// The threads as a table `width` columns wide: title, folder, age, status and id.
fn table(threads: &[ThreadInfo], now_ms: u64, width: usize) -> String {
    if threads.is_empty() {
        return "No threads.\n".to_owned();
    }
    let rows: Vec<Vec<String>> = threads
        .iter()
        .map(|thread| {
            vec![
                thread_title(thread).to_owned(),
                thread.chat.folder.clone(),
                age_label(thread.updated_ms, now_ms),
                thread_status(thread).to_owned(),
                thread.chat.id.clone(),
            ]
        })
        .collect();
    human::table(
        &[
            ("THREAD", Fit::End),
            ("FOLDER", Fit::Start),
            ("AGE", Fit::Keep),
            ("STATUS", Fit::Keep),
            ("ID", Fit::Keep),
        ],
        &rows,
        width,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::ContentEnvironment;
    use compi_protocol::chat::{ChatHarness, ThreadActivity};

    #[test]
    fn the_table_names_each_thread_with_its_id() {
        let mut chat = crate::chat::new_chat(
            ChatHarness::Omp,
            ContentEnvironment::Host,
            "/work/compi".into(),
            None,
        );
        chat.id = "chat-1".into();
        let now = 10 * 3_600_000;
        let threads = [ThreadInfo {
            chat,
            title: Some("Fix the build".into()),
            created_ms: 0,
            updated_ms: now - 3 * 3_600_000,
            activity: ThreadActivity::Idle,
            project: None,
            open: false,
        }];
        assert_eq!(
            table(&threads, now, 100).lines().collect::<Vec<_>>(),
            [
                "THREAD         FOLDER       AGE  STATUS  ID",
                "Fix the build  /work/compi  3h   Ended   chat-1",
            ]
        );
        assert_eq!(table(&[], now, 100), "No threads.\n");
    }
}
