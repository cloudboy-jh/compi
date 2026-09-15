use base64::{Engine, engine::general_purpose::STANDARD};
use compi_client::{MirrorApply, ScreenMirror};
use compi_daemon::screen;
use compi_daemon::terminal::TerminalState;
use compi_protocol::ScreenMessage;
use serde_json::Value;

fn expected_screen(fixture: &Value) -> ScreenMessage {
    serde_json::from_value(fixture["value"].clone()).unwrap()
}

fn without_placements(mut message: ScreenMessage) -> ScreenMessage {
    match &mut message {
        ScreenMessage::Snapshot { snapshot } => snapshot.placements.clear(),
        ScreenMessage::Delta { delta } => delta.placements = None,
    }
    message
}

#[test]
fn extracted_engine_and_replica_match_pre_extraction_replay_except_graphics_reflow() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../compi-protocol/tests/fixtures/terminal-v7.json"
    ))
    .unwrap();
    let mut terminal = TerminalState::new(
        fixture["cols"].as_u64().unwrap() as u16,
        fixture["rows"].as_u64().unwrap() as u16,
    );
    let initial = ScreenMessage::Snapshot {
        snapshot: screen::snapshot(terminal.snapshot()),
    };
    assert_eq!(initial, expected_screen(&fixture["initial"]));
    let mut replica = ScreenMirror::default();
    assert_eq!(replica.apply(initial), MirrorApply::Applied);
    let mut graphics_reflowed = false;

    for event in fixture["events"].as_array().unwrap() {
        graphics_reflowed |= event.get("resize").is_some();
        let (delta, replies) = if let Some(output) = event.get("output") {
            terminal.advance(&STANDARD.decode(output.as_str().unwrap()).unwrap())
        } else {
            let dimensions = event["resize"].as_array().unwrap();
            (
                terminal.resize(
                    dimensions[0].as_u64().unwrap() as u16,
                    dimensions[1].as_u64().unwrap() as u16,
                ),
                Vec::new(),
            )
        };
        let expected_replies: Vec<Vec<u8>> =
            serde_json::from_value(event["replies"].clone()).unwrap();
        assert_eq!(replies, expected_replies);
        let message = delta.map(|delta| ScreenMessage::Delta {
            delta: screen::delta(delta),
        });
        let expected_delta = (!event["delta"].is_null()).then(|| expected_screen(&event["delta"]));
        if graphics_reflowed {
            assert_eq!(
                message.clone().map(without_placements),
                expected_delta.map(without_placements)
            );
        } else {
            assert_eq!(message, expected_delta);
        }
        if let Some(message) = message {
            assert_eq!(replica.apply(message), MirrorApply::Applied);
        }
        let snapshot = screen::snapshot(terminal.snapshot());
        let actual_snapshot = ScreenMessage::Snapshot {
            snapshot: snapshot.clone(),
        };
        let expected_snapshot = expected_screen(&event["snapshot"]);
        if graphics_reflowed {
            assert_eq!(
                without_placements(actual_snapshot),
                without_placements(expected_snapshot)
            );
        } else {
            assert_eq!(actual_snapshot, expected_snapshot);
        }
        assert_eq!(replica.snapshot(), Some(&snapshot));
    }
}

#[test]
fn sequence_gap_preserves_known_state_until_snapshot_recovery() {
    let mut terminal = TerminalState::new(10, 2);
    let initial = screen::snapshot(terminal.snapshot());
    let mut replica = ScreenMirror::default();
    replica.apply(ScreenMessage::Snapshot {
        snapshot: initial.clone(),
    });
    terminal.advance(b"first");
    let (second, _) = terminal.advance(b"second");
    assert_eq!(
        replica.apply(ScreenMessage::Delta {
            delta: screen::delta(second.unwrap()),
        }),
        MirrorApply::Gap {
            expected: 1,
            actual: 2,
        }
    );
    assert_eq!(replica.snapshot(), Some(&initial));
    let current = screen::snapshot(terminal.snapshot());
    assert_eq!(
        replica.apply(ScreenMessage::Snapshot {
            snapshot: current.clone(),
        }),
        MirrorApply::Applied
    );
    assert_eq!(replica.snapshot(), Some(&current));
    let resized = terminal.resize(5, 3).unwrap();
    assert_eq!(
        replica.apply(ScreenMessage::Delta {
            delta: screen::delta(resized),
        }),
        MirrorApply::Applied
    );
    assert_eq!(
        replica.snapshot(),
        Some(&screen::snapshot(terminal.snapshot()))
    );
}

#[test]
fn image_anchor_tracks_retained_history_without_retransmitting_pixels() {
    let mut terminal = TerminalState::new(10, 2);
    terminal.set_resource_limits(2, 8);
    terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=1,p=7;AQIDBA==\x1b\\");
    let initial = screen::snapshot(terminal.snapshot());
    let mut replica = ScreenMirror::default();
    replica.apply(ScreenMessage::Snapshot {
        snapshot: initial.clone(),
    });
    for (output, expected_row) in [
        (b"one\r\ntwo\r\nthree".as_slice(), Some(-1)),
        (b"\r\nfour".as_slice(), Some(-2)),
        (b"\r\nfive".as_slice(), None),
    ] {
        let delta = terminal.advance(output).0.unwrap();
        assert!(
            delta.images.is_none(),
            "moving an anchor must not resend its pixels"
        );
        assert_eq!(
            replica.apply(ScreenMessage::Delta {
                delta: screen::delta(delta)
            }),
            MirrorApply::Applied
        );
        let authoritative = screen::snapshot(terminal.snapshot());
        assert_eq!(
            authoritative
                .placements
                .first()
                .map(|placement| placement.row),
            expected_row
        );
        assert_eq!(authoritative.images, initial.images);
        assert_eq!(replica.snapshot(), Some(&authoritative));
    }
    // Once the anchor's history is gone, its unreferenced pixels may be reclaimed.
    let (delta, replies) = terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=2;BQYHCA==\x1b\\");
    assert!(replies.is_empty());
    assert_eq!(
        replica.apply(ScreenMessage::Delta {
            delta: screen::delta(delta.unwrap())
        }),
        MirrorApply::Applied
    );
    assert_eq!(replica.snapshot().unwrap().images[0].id, 2);
    assert_eq!(
        replica.snapshot(),
        Some(&screen::snapshot(terminal.snapshot()))
    );
}

#[test]
fn resize_reflows_text_and_logical_image_anchor_without_retransmitting_payload() {
    let mut terminal = TerminalState::new(10, 3);
    terminal.advance(b"abcdefghijK\x1b_Ga=T,f=32,s=1,v=1,i=1,p=7;AQIDBA==\x1b\\");
    let initial = screen::snapshot(terminal.snapshot());
    let mut replica = ScreenMirror::default();
    replica.apply(ScreenMessage::Snapshot {
        snapshot: initial.clone(),
    });
    let delta = terminal.resize(5, 3).unwrap();
    assert_eq!(
        replica.apply(ScreenMessage::Delta {
            delta: screen::delta(delta)
        }),
        MirrorApply::Applied
    );
    let resized = screen::snapshot(terminal.snapshot());
    assert_eq!(resized.images, initial.images);
    assert_eq!(resized.placements[0].row, 2);
    assert!(resized.scrollback.is_empty());
    assert_eq!(
        resized.cells[0]
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>(),
        "abcde"
    );
    assert_eq!(replica.snapshot(), Some(&resized));
    terminal.clear_scrollback();
    // Clearing history does not remove the placement while its reflowed anchor remains on-screen.
    assert_eq!(terminal.snapshot().placements, resized.placements);
}

#[test]
fn clearing_history_removes_only_history_image_anchors() {
    let mut terminal = TerminalState::new(10, 2);
    terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;AQIDBA==\x1b\\one\r\ntwo\r\nthree");
    terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=2;BQYHCA==\x1b\\");
    let before = terminal.snapshot();
    assert_eq!(
        before
            .placements
            .iter()
            .map(|placement| placement.row)
            .collect::<Vec<_>>(),
        [-1, 1]
    );
    terminal.clear_scrollback();
    let after = terminal.snapshot();
    assert_eq!(after.images, before.images);
    assert_eq!(after.placements, vec![before.placements[1]]);
}
