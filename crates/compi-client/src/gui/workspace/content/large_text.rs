//! Text files too large for the editor, shown read-only one part at a time: about 1 MiB
//! cut at line breaks, read from the daemon on a background thread. Only the shown part
//! is in memory; Start, Previous, Next and End move through the file.

use super::*;
use base64::Engine as _;

/// About how much of a large text file shows at once.
const PART_BYTES: u64 = 1024 * 1024;

static NEXT_PART: AtomicU64 = AtomicU64::new(1);

/// A move through a large text file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Start,
    Previous,
    Next,
    End,
}

/// A part as read: its bytes' place in the file, the file's length and modification time
/// then, and its text.
pub(in crate::gui) struct Part {
    start: u64,
    end: u64,
    len: u64,
    modified: u64,
    text: String,
}

/// A large text file's tab.
pub(super) struct LargeText {
    len: u64,
    /// The shown part's place in the file.
    shown: Option<(u64, u64)>,
    /// The file's length and modification time at its first part.
    stamp: Option<(u64, u64)>,
    /// A later part found the file different from its first.
    changed: bool,
    /// A part that landed, waiting for a window to build its editor.
    arrived: Option<Part>,
    pub(super) editor: Option<Entity<EditorState>>,
    error: Option<String>,
    /// The read this waits for, if one is under way.
    request: Option<u64>,
}

impl LargeText {
    pub(super) fn new(len: u64) -> Self {
        Self {
            len,
            shown: None,
            stamp: None,
            changed: false,
            arrived: None,
            editor: None,
            error: None,
            request: None,
        }
    }

    /// Whether `step` goes anywhere from the shown part.
    fn can(&self, step: Step) -> bool {
        match (step, self.shown) {
            (_, None) => false,
            (Step::Start | Step::Previous, Some((start, _))) => start > 0,
            (Step::Next | Step::End, Some((_, end))) => end < self.len,
        }
    }

    /// Takes a part read for `request`; true when it waits for its editor.
    fn accept(&mut self, request: u64, result: Result<Part, String>) -> bool {
        if self.request != Some(request) {
            return false;
        }
        self.request = None;
        match result {
            Ok(part) => {
                let stamp = (part.len, part.modified);
                self.changed |= self.stamp.is_some_and(|first| first != stamp);
                self.stamp.get_or_insert(stamp);
                self.len = part.len;
                self.error = None;
                self.arrived = Some(part);
                true
            }
            Err(error) => {
                self.error = Some(error);
                false
            }
        }
    }

    /// Builds the read-only editor of a part that arrived. Line numbers are the file's
    /// only in its first part.
    pub(super) fn build(&mut self, window: &mut Window, cx: &mut App) {
        let Some(part) = self.arrived.take() else {
            return;
        };
        let first = part.start == 0;
        self.shown = Some((part.start, part.end));
        self.editor = Some(cx.new(|cx| {
            EditorState::new(window, cx)
                .language("text")
                .soft_wrap(false)
                .line_number(first)
                .default_value(part.text)
        }));
    }
}

/// Where a part read forward from `offset` ends within `bytes`: after its last line break,
/// all of them at the end of a file `len` long, or before a character the read cut off.
fn forward_cut(offset: u64, bytes: &[u8], len: u64) -> usize {
    if offset + bytes.len() as u64 >= len {
        return bytes.len();
    }
    match bytes.iter().rposition(|&byte| byte == b'\n') {
        Some(at) => at + 1,
        None => match std::str::from_utf8(bytes) {
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            _ => bytes.len(),
        },
    }
}

/// Where a part read backward from `from`, its `bytes` ending where the next part starts,
/// begins within them: at the file's start, after its first line break, or past a
/// character the read cut off.
fn backward_cut(from: u64, bytes: &[u8]) -> usize {
    if from == 0 {
        return 0;
    }
    match bytes.iter().position(|&byte| byte == b'\n') {
        Some(at) if at + 1 < bytes.len() => at + 1,
        _ => bytes
            .iter()
            .take(3)
            .take_while(|&&byte| byte & 0xc0 == 0x80)
            .count(),
    }
}

/// Reads the part `step` goes to from `shown` in the file at `path`.
fn read_part(
    target: &ConnectionTarget,
    environment: &ContentEnvironment,
    path: &str,
    step: Step,
    shown: Option<(u64, u64)>,
) -> Result<Part, String> {
    let mut client = target.connect().map_err(|error| error.to_string())?;
    let mut read = |offset: u64, len: u64| {
        let range = client
            .read_content_range(environment.clone(), path.to_owned(), offset, len)
            .map_err(|error| error.to_string())?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&range.base64)
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((range, bytes))
    };
    let forward = |(range, bytes): (compi_protocol::ContentRange, Vec<u8>)| {
        let cut = forward_cut(range.offset, &bytes, range.len);
        Part {
            start: range.offset,
            end: range.offset + cut as u64,
            len: range.len,
            modified: range.modified,
            text: String::from_utf8_lossy(&bytes[..cut]).into_owned(),
        }
    };
    let backward = |(range, bytes): (compi_protocol::ContentRange, Vec<u8>)| {
        let cut = backward_cut(range.offset, &bytes);
        Part {
            start: range.offset + cut as u64,
            end: range.offset + bytes.len() as u64,
            len: range.len,
            modified: range.modified,
            text: String::from_utf8_lossy(&bytes[cut..]).into_owned(),
        }
    };
    let (start, end) = shown.unwrap_or((0, 0));
    Ok(match step {
        Step::Start => forward(read(0, PART_BYTES)?),
        Step::Next => forward(read(end, PART_BYTES)?),
        Step::Previous => {
            let from = start.saturating_sub(PART_BYTES);
            backward(read(from, start - from)?)
        }
        Step::End => {
            let len = read(0, 0)?.0.len;
            let from = len.saturating_sub(PART_BYTES);
            backward(read(from, len - from)?)
        }
    })
}

impl CompiApp {
    /// Reads the part of large text file `path` in `pane_id` that `step` goes to.
    pub(super) fn step_large_text(&mut self, pane_id: &PaneId, path: &str, step: Step) {
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let environment = ui.environment.clone();
        let Some(OpenFile {
            body: FileBody::LargeText(large),
            ..
        }) = ui.files.get_mut(path)
        else {
            return;
        };
        if large.shown.is_some() && !large.can(step) {
            return;
        }
        let request = NEXT_PART.fetch_add(1, Ordering::Relaxed);
        large.request = Some(request);
        let shown = large.shown;
        let (target, sender) = (self.target.clone(), self.event_tx.clone());
        let (pane_id, path) = (pane_id.clone(), path.to_owned());
        thread::spawn(move || {
            let result = read_part(&target, &environment, &path, step, shown);
            sender.send(UiEvent::LargeTextPart {
                pane_id,
                path,
                request,
                result,
            });
        });
    }

    /// A part of a large text file, or why it could not be read.
    pub(in crate::gui) fn large_text_part(
        &mut self,
        pane_id: &PaneId,
        path: &str,
        request: u64,
        result: Result<Part, String>,
    ) {
        if let Some(OpenFile {
            body: FileBody::LargeText(large),
            ..
        }) = self
            .content
            .panes
            .get_mut(pane_id)
            .and_then(|ui| ui.files.get_mut(path))
            && large.accept(request, result)
        {
            self.content.unbuilt = true;
        }
    }

    /// A large text file's body: a bar saying which part shows, with Start, Previous,
    /// Next and End, over the part in a read-only editor.
    pub(super) fn render_large_text(
        &self,
        pane_id: &PaneId,
        path: &str,
        large: &LargeText,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let place = match large.shown {
            Some((start, end)) => format!(
                "{} to {} of {}",
                human_size(start),
                human_size(end),
                human_size(large.len)
            ),
            None => human_size(large.len),
        };
        let mut info = format!(
            "{place} · read-only, larger than the {} MiB editor limit",
            MAX_CONTENT_FILE_BYTES / (1024 * 1024)
        );
        if large.changed {
            info.push_str(" · changed on disk since it opened");
        }
        if let Some(error) = &large.error {
            info = format!("{info} · {error}");
        } else if large.request.is_some() {
            info.push_str(" · reading…");
        }
        let button = |id: &'static str, label: &'static str, tooltip: &'static str, step: Step| {
            let (pane, path) = (pane_id.clone(), path.to_owned());
            text_button(id, label)
                .tooltip(tooltip)
                .header()
                .disabled_because((!large.can(step)).then_some(match step {
                    Step::Start | Step::Previous => "This is the start of the file",
                    Step::Next | Step::End => "This is the end of the file",
                }))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.step_large_text(&pane, &path, step);
                    cx.notify();
                }))
        };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(color(colors.border))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .text_color(color(colors.muted))
                            .child(info),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(button(
                                "large-start",
                                "Start",
                                "The first part",
                                Step::Start,
                            ))
                            .child(button(
                                "large-previous",
                                "Previous",
                                "The part before this one",
                                Step::Previous,
                            ))
                            .child(button(
                                "large-next",
                                "Next",
                                "The part after this one",
                                Step::Next,
                            ))
                            .child(button("large-end", "End", "The last part", Step::End)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .children(large.editor.as_ref().map(|editor| {
                        Editor::new(editor)
                            .appearance(false)
                            .bordered(false)
                            .readonly(true)
                            .font_family(self.typography.font.family.clone())
                            .text_size(px(self.typography.font_size))
                            .size_full()
                    })),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_parts_end_after_a_line_break_or_before_a_cut_character() {
        // Mid-file: up to and with the last line break.
        assert_eq!(forward_cut(0, b"one\ntwo\nthr", 100), 8);
        // At the end of the file: everything.
        assert_eq!(forward_cut(89, b"one\ntwo\nthr", 100), 11);
        // One long line: whole characters only, the rest left for the next part.
        assert_eq!(forward_cut(0, "aé".as_bytes(), 100), 3);
        assert_eq!(forward_cut(0, &"aé".as_bytes()[..2], 100), 1);
    }

    #[test]
    fn backward_parts_start_after_a_line_break_or_past_a_cut_character() {
        assert_eq!(backward_cut(0, b"partial\nline"), 0);
        assert_eq!(backward_cut(10, b"tial\nline\nend"), 5);
        // A line break only at the very end does not leave the part empty.
        assert_eq!(backward_cut(10, b"no break here\n"), 0);
        // One long line: from the first whole character.
        assert_eq!(backward_cut(10, &"éa".as_bytes()[1..]), 1);
    }

    #[test]
    fn steps_stop_at_the_ends_of_the_file() {
        let mut large = LargeText::new(3_000_000);
        assert!(!large.can(Step::Next));
        large.shown = Some((0, 1_000_000));
        assert!(!large.can(Step::Previous) && !large.can(Step::Start));
        assert!(large.can(Step::Next) && large.can(Step::End));
        large.shown = Some((2_000_000, 3_000_000));
        assert!(large.can(Step::Previous) && !large.can(Step::Next));
    }
}
