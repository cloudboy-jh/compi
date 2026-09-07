use compi_protocol::MouseMode;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Key<'a> {
    pub modifiers: Modifiers,
    pub key: &'a str,
    pub key_char: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeypadKey {
    Digit(u8),
    Decimal,
    Divide,
    Multiply,
    Subtract,
    Add,
}

pub fn encode_keystroke(
    keystroke: &Key<'_>,
    application_cursor: bool,
    keypad: Option<KeypadKey>,
) -> Option<Vec<u8>> {
    if let Some(keypad) = keypad {
        return Some(encode_application_keypad(keypad));
    }

    let key = keystroke.key;
    let modifier = xterm_modifier(&keystroke.modifiers);
    let special = match key {
        "tab" if keystroke.modifiers.shift => Some(if modifier == 2 {
            b"\x1b[Z".to_vec()
        } else {
            format!("\x1b[1;{modifier}Z").into_bytes()
        }),
        "up" => Some(encode_cursor_key(b'A', application_cursor, modifier)),
        "down" => Some(encode_cursor_key(b'B', application_cursor, modifier)),
        "right" => Some(encode_cursor_key(b'C', application_cursor, modifier)),
        "left" => Some(encode_cursor_key(b'D', application_cursor, modifier)),
        "home" => Some(encode_cursor_key(b'H', application_cursor, modifier)),
        "end" => Some(encode_cursor_key(b'F', application_cursor, modifier)),
        "insert" => Some(encode_tilde_key(2, modifier)),
        "delete" => Some(encode_tilde_key(3, modifier)),
        "pageup" => Some(encode_tilde_key(5, modifier)),
        "pagedown" => Some(encode_tilde_key(6, modifier)),
        "f1" => Some(encode_function_key(b'P', modifier)),
        "f2" => Some(encode_function_key(b'Q', modifier)),
        "f3" => Some(encode_function_key(b'R', modifier)),
        "f4" => Some(encode_function_key(b'S', modifier)),
        "f5" => Some(encode_tilde_key(15, modifier)),
        "f6" => Some(encode_tilde_key(17, modifier)),
        "f7" => Some(encode_tilde_key(18, modifier)),
        "f8" => Some(encode_tilde_key(19, modifier)),
        "f9" => Some(encode_tilde_key(20, modifier)),
        "f10" => Some(encode_tilde_key(21, modifier)),
        "f11" => Some(encode_tilde_key(23, modifier)),
        "f12" => Some(encode_tilde_key(24, modifier)),
        _ => None,
    };
    if special.is_some() {
        return special;
    }

    let control;
    let text = if keystroke.modifiers.control {
        control = [control_byte(key)?];
        control.as_slice()
    } else {
        match key {
            "enter" => b"\r".as_slice(),
            "tab" => b"\t".as_slice(),
            "space" => b" ".as_slice(),
            "backspace" => b"\x7f".as_slice(),
            "escape" => b"\x1b".as_slice(),
            _ => keystroke.key_char?.as_bytes(),
        }
    };
    let mut bytes = Vec::with_capacity(text.len() + usize::from(keystroke.modifiers.alt));
    if keystroke.modifiers.alt {
        bytes.push(0x1b);
    }
    bytes.extend_from_slice(text);
    Some(bytes)
}

fn encode_cursor_key(key: u8, application_cursor: bool, modifier: u8) -> Vec<u8> {
    if modifier > 1 {
        format!("\x1b[1;{modifier}{}", key as char).into_bytes()
    } else if application_cursor {
        vec![0x1b, b'O', key]
    } else {
        vec![0x1b, b'[', key]
    }
}

fn encode_function_key(key: u8, modifier: u8) -> Vec<u8> {
    if modifier > 1 {
        format!("\x1b[1;{modifier}{}", key as char).into_bytes()
    } else {
        vec![0x1b, b'O', key]
    }
}

fn encode_tilde_key(key: u8, modifier: u8) -> Vec<u8> {
    if modifier > 1 {
        format!("\x1b[{key};{modifier}~").into_bytes()
    } else {
        format!("\x1b[{key}~").into_bytes()
    }
}

fn encode_application_keypad(key: KeypadKey) -> Vec<u8> {
    let key = match key {
        KeypadKey::Digit(0) => b'p',
        KeypadKey::Digit(1) => b'q',
        KeypadKey::Digit(2) => b'r',
        KeypadKey::Digit(3) => b's',
        KeypadKey::Digit(4) => b't',
        KeypadKey::Digit(5) => b'u',
        KeypadKey::Digit(6) => b'v',
        KeypadKey::Digit(7) => b'w',
        KeypadKey::Digit(8) => b'x',
        KeypadKey::Digit(9) => b'y',
        KeypadKey::Digit(_) => unreachable!(),
        KeypadKey::Decimal => b'n',
        KeypadKey::Divide => b'o',
        KeypadKey::Multiply => b'j',
        KeypadKey::Subtract => b'm',
        KeypadKey::Add => b'k',
    };
    vec![0x1b, b'O', key]
}

fn xterm_modifier(modifiers: &Modifiers) -> u8 {
    1 + u8::from(modifiers.shift) + 2 * u8::from(modifiers.alt) + 4 * u8::from(modifiers.control)
}

fn control_byte(key: &str) -> Option<u8> {
    if key == "space" {
        return Some(0);
    }
    let [byte] = key.as_bytes() else {
        return None;
    };
    match byte.to_ascii_uppercase() {
        b'@' => Some(0),
        byte @ b'A'..=b'Z' => Some(byte & 0x1f),
        byte @ b'['..=b'_' => Some(byte & 0x1f),
        b'?' => Some(0x7f),
        _ => None,
    }
}

pub fn is_application_shortcut(keystroke: &Key<'_>) -> bool {
    if !keystroke.modifiers.control {
        return false;
    }
    matches!(
        (keystroke.modifiers.shift, keystroke.key),
        (false, "c" | "t" | "v" | "w" | "tab") | (true, "tab" | "c" | "v" | "p")
    )
}

pub fn utf16_byte_index(text: &str, target: usize) -> usize {
    let mut utf16_offset = 0;
    for (byte_offset, character) in text.char_indices() {
        if utf16_offset >= target {
            return byte_offset;
        }
        utf16_offset += character.len_utf16();
        if utf16_offset >= target {
            return byte_offset + character.len_utf8();
        }
    }
    text.len()
}

pub fn encode_mouse(
    button: Option<MouseButton>,
    release: bool,
    motion: bool,
    col: usize,
    row: usize,
    modifiers: Modifiers,
) -> Option<Vec<u8>> {
    let mut code = match button {
        Some(MouseButton::Left) => 0,
        Some(MouseButton::Middle) => 1,
        Some(MouseButton::Right) => 2,
        None => 3,
        Some(MouseButton::Unsupported) => return None,
    };
    if motion {
        code += 32;
    }
    Some(encode_sgr_mouse_code(code, col, row, modifiers, !release))
}

pub fn encode_sgr_mouse_code(
    mut code: u8,
    col: usize,
    row: usize,
    modifiers: Modifiers,
    press: bool,
) -> Vec<u8> {
    if modifiers.shift {
        code += 4;
    }
    if modifiers.alt {
        code += 8;
    }
    if modifiers.control {
        code += 16;
    }
    format!(
        "\x1b[<{code};{};{}{}",
        col.saturating_add(1),
        row.saturating_add(1),
        if press { 'M' } else { 'm' }
    )
    .into_bytes()
}

pub fn reports_mouse_motion(mode: MouseMode, button_pressed: bool) -> bool {
    match mode {
        MouseMode::None | MouseMode::Normal => false,
        MouseMode::ButtonMotion => button_pressed,
        MouseMode::AnyMotion => true,
    }
}

pub fn encode_mouse_wheel(up: bool, col: usize, row: usize, modifiers: Modifiers) -> Vec<u8> {
    encode_sgr_mouse_code(if up { 64 } else { 65 }, col, row, modifiers, true)
}

pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let mut data = Vec::with_capacity(text.len() + if bracketed { 12 } else { 0 });
    if bracketed {
        data.extend_from_slice(b"\x1b[200~");
    }
    let mut bytes = text.bytes().peekable();
    while let Some(byte) = bytes.next() {
        if byte == b'\r' && bytes.peek() == Some(&b'\n') {
            continue;
        }
        data.push(if byte == b'\n' { b'\r' } else { byte });
    }
    if bracketed {
        data.extend_from_slice(b"\x1b[201~");
    }
    data
}

pub fn encode_focus(focused: bool) -> Vec<u8> {
    if focused {
        b"\x1b[I".to_vec()
    } else {
        b"\x1b[O".to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_utf16_at_scalar_boundaries() {
        let text = "a😀é";
        assert_eq!(utf16_byte_index(text, 0), 0);
        assert_eq!(utf16_byte_index(text, 1), 1);
        assert_eq!(utf16_byte_index(text, 2), 5);
        assert_eq!(utf16_byte_index(text, 3), 5);
        assert_eq!(utf16_byte_index(text, 4), 7);
        assert_eq!(utf16_byte_index(text, usize::MAX), 7);
    }

    #[test]
    fn normalizes_paste_line_endings_inside_brackets() {
        let text = "a\r\nb\nc\rd";
        assert_eq!(encode_paste(text, false), b"a\rb\rc\rd");
        assert_eq!(encode_paste(text, true), b"\x1b[200~a\rb\rc\rd\x1b[201~");
    }

    #[test]
    fn maps_terminal_control_modified_and_keypad_keys() {
        let ctrl_c = Key {
            modifiers: Modifiers {
                control: true,
                ..Default::default()
            },
            key: "c",
            key_char: None,
        };
        assert_eq!(encode_keystroke(&ctrl_c, false, None), Some(vec![3]));
        let space = Key {
            key: "space",
            ..Default::default()
        };
        assert_eq!(encode_keystroke(&space, false, None), Some(b" ".to_vec()));
        let up = Key {
            key: "up",
            ..Default::default()
        };
        assert_eq!(encode_keystroke(&up, false, None), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_keystroke(&up, true, None), Some(b"\x1bOA".to_vec()));
        let application_home = Key {
            key: "home",
            ..Default::default()
        };
        assert_eq!(
            encode_keystroke(&application_home, true, None),
            Some(b"\x1bOH".to_vec())
        );
        let modified_up = Key {
            modifiers: Modifiers {
                control: true,
                shift: true,
                ..Default::default()
            },
            key: "up",
            key_char: None,
        };
        assert_eq!(
            encode_keystroke(&modified_up, true, None),
            Some(b"\x1b[1;6A".to_vec())
        );
        let ctrl_space = Key {
            modifiers: Modifiers {
                control: true,
                ..Default::default()
            },
            key: "space",
            key_char: None,
        };
        assert_eq!(encode_keystroke(&ctrl_space, false, None), Some(vec![0]));
        assert_eq!(
            encode_keystroke(&space, false, Some(KeypadKey::Digit(7))),
            Some(b"\x1bOw".to_vec())
        );
    }

    #[test]
    fn reserves_clipboard_and_paste_shortcuts() {
        fn shortcut(key: &str, shift: bool) -> Key<'_> {
            Key {
                modifiers: Modifiers {
                    control: true,
                    shift,
                    ..Default::default()
                },
                key,
                key_char: None,
            }
        }

        assert!(is_application_shortcut(&shortcut("v", false)));
        assert!(is_application_shortcut(&shortcut("v", true)));
        assert!(is_application_shortcut(&shortcut("c", false)));
        assert!(is_application_shortcut(&shortcut("c", true)));
    }

    #[test]
    fn encodes_sgr_mouse_coordinates_and_modifiers() {
        let data = encode_sgr_mouse_code(
            0,
            2,
            4,
            Modifiers {
                control: true,
                ..Default::default()
            },
            true,
        );

        assert_eq!(data, b"\x1b[<16;3;5M");
    }
    #[test]
    fn encodes_any_motion_without_a_pressed_button() {
        let data = encode_mouse(None, false, true, 0, 0, Modifiers::default());
        assert_eq!(data.as_deref(), Some(b"\x1b[<35;1;1M".as_slice()));
    }
}
