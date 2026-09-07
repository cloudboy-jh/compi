use base64::{Engine, engine::general_purpose::STANDARD};
use compi_protocol::{SCREEN_FRAME, ScreenMessage, decode_screen, encode_screen, frame};
use serde_json::Value;
use std::io::Cursor;

// Captured with the original v7 codecs before crate extraction. These fixtures pin
// the peer-visible bytes, not just agreement between the current encoder/decoder.
fn fixture_frame(fixture: &Value, kind: u8) -> (Vec<u8>, Vec<u8>) {
    let original = STANDARD.decode(fixture["frame"].as_str().unwrap()).unwrap();
    let mut reader = Cursor::new(&original);
    let decoded = frame::read(&mut reader).unwrap().unwrap();
    assert_eq!(decoded.kind, kind);
    assert_eq!(reader.position() as usize, original.len());
    (original, decoded.payload)
}

fn assert_frame(original: &[u8], kind: u8, payload: &[u8]) {
    let mut encoded = Vec::new();
    frame::write(&mut encoded, kind, payload).unwrap();
    assert_eq!(encoded, original);
}

#[test]
fn captured_v7_control_frames_remain_decodable_json() {
    let fixtures: Value = serde_json::from_str(include_str!("fixtures/control-v7.json")).unwrap();
    for fixture in fixtures["client"]
        .as_array()
        .unwrap()
        .iter()
        .chain(fixtures["server"].as_array().unwrap())
    {
        let (original, payload) = fixture_frame(fixture, 1);
        let expected = fixture["value"].clone();
        let decoded: Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(decoded, expected);
        assert_frame(&original, 1, &serde_json::to_vec(&decoded).unwrap());
    }
}

#[test]
fn screen_peers_preserve_pre_extraction_v7_bytes() {
    let fixtures: Value = serde_json::from_str(include_str!("fixtures/terminal-v7.json")).unwrap();
    let screens = std::iter::once(&fixtures["initial"]).chain(
        fixtures["events"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|event| [&event["delta"], &event["snapshot"]])
            .filter(|fixture| !fixture.is_null()),
    );
    for fixture in screens {
        let (original, payload) = fixture_frame(fixture, SCREEN_FRAME);
        let expected: ScreenMessage = serde_json::from_value(fixture["value"].clone()).unwrap();
        assert_eq!(decode_screen(&payload).unwrap(), expected);
        assert_frame(&original, SCREEN_FRAME, &encode_screen(&expected).unwrap());
    }
}
