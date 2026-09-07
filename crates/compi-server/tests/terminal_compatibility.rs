use base64::{Engine, engine::general_purpose::STANDARD};
use compi_client_core::{MirrorApply, ScreenMirror};
use compi_protocol::ScreenMessage;
use compi_server::screen;
use compi_terminal::TerminalState;
use serde_json::Value;

fn expected_screen(fixture: &Value) -> ScreenMessage {
    serde_json::from_value(fixture["value"].clone()).unwrap()
}

#[test]
fn extracted_engine_and_replica_match_pre_extraction_replay() {
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

    for event in fixture["events"].as_array().unwrap() {
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
        assert_eq!(message, expected_delta);
        if let Some(message) = message {
            assert_eq!(replica.apply(message), MirrorApply::Applied);
        }
        let snapshot = screen::snapshot(terminal.snapshot());
        assert_eq!(
            ScreenMessage::Snapshot {
                snapshot: snapshot.clone(),
            },
            expected_screen(&event["snapshot"])
        );
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
