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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WheelRoute {
    #[default]
    History,
    Mouse,
    Alternate,
}

impl WheelRoute {
    pub fn for_modes(mouse: MouseMode, alternate_screen: bool, shift: bool) -> Self {
        if shift {
            Self::History
        } else if mouse != MouseMode::None {
            Self::Mouse
        } else if alternate_screen {
            Self::Alternate
        } else {
            Self::History
        }
    }
}

#[derive(Debug, Default)]
pub struct WheelAccumulator {
    route: WheelRoute,
    remainder: f64,
}

impl WheelAccumulator {
    pub fn take_lines(&mut self, lines: f64, route: WheelRoute) -> isize {
        if self.route != route {
            self.route = route;
            self.remainder = 0.0;
        }
        self.remainder += lines;
        let whole = self.remainder.trunc() as isize;
        self.remainder -= whole as f64;
        whole
    }
}

pub fn encode_keystroke(
    keystroke: &Key<'_>,
    application_cursor: bool,
    keypad: Option<KeypadKey>,
    keyboard_protocol_flags: u8,
) -> Option<Vec<u8>> {
    if let Some(keypad) = keypad {
        return Some(encode_application_keypad(keypad));
    }

    // Legacy terminals cannot distinguish Ctrl+Enter from Enter. Only send
    // CSI u once the terminal application enables Kitty's disambiguation flag.
    if keystroke.key == "enter" && keystroke.modifiers.control {
        return Some(if keyboard_protocol_flags & 1 != 0 {
            format!("\x1b[13;{}u", xterm_modifier(&keystroke.modifiers)).into_bytes()
        } else if keystroke.modifiers.alt {
            b"\x1b\r".to_vec()
        } else {
            b"\r".to_vec()
        });
    }
    if keystroke.key == "backspace" && keystroke.modifiers.control {
        let mut bytes = Vec::with_capacity(1 + usize::from(keystroke.modifiers.alt));
        if keystroke.modifiers.alt {
            bytes.push(0x1b);
        }
        bytes.push(0x17);
        return Some(bytes);
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

pub fn encode_mouse_wheel(
    up: bool,
    lines: usize,
    col: usize,
    row: usize,
    modifiers: Modifiers,
) -> Vec<u8> {
    if lines == 0 {
        return Vec::new();
    }
    let mut data = encode_sgr_mouse_code(if up { 64 } else { 65 }, col, row, modifiers, true);
    let report_len = data.len();
    data.reserve(report_len * (lines - 1));
    for _ in 1..lines {
        data.extend_from_within(..report_len);
    }
    data
}

pub fn encode_alternate_scroll(up: bool, lines: usize, application_cursor: bool) -> Vec<u8> {
    let arrow: &[u8] = match (up, application_cursor) {
        (true, false) => b"\x1b[A",
        (true, true) => b"\x1bOA",
        (false, false) => b"\x1b[B",
        (false, true) => b"\x1bOB",
    };
    arrow.repeat(lines)
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
    fn wheel_accumulates_signed_fractions_and_cancels_opposite_motion() {
        let mut wheel = WheelAccumulator::default();
        assert_eq!(wheel.take_lines(0.75, WheelRoute::History), 0);
        assert_eq!(wheel.take_lines(-0.5, WheelRoute::History), 0);
        assert_eq!(wheel.take_lines(0.75, WheelRoute::History), 1);
        assert_eq!(wheel.take_lines(0.0, WheelRoute::History), 0);
        assert_eq!(wheel.take_lines(-2.5, WheelRoute::History), -2);
        assert_eq!(wheel.take_lines(-0.5, WheelRoute::History), -1);
        assert_eq!(wheel.take_lines(3.5, WheelRoute::History), 3);
        assert_eq!(wheel.take_lines(0.5, WheelRoute::History), 1);
    }

    #[test]
    fn wheel_fraction_is_isolated_by_pane_and_destination() {
        let mut first = WheelAccumulator::default();
        let mut second = WheelAccumulator::default();
        assert_eq!(first.take_lines(0.75, WheelRoute::History), 0);
        assert_eq!(second.take_lines(0.5, WheelRoute::History), 0);
        assert_eq!(first.take_lines(0.5, WheelRoute::History), 1);
        assert_eq!(second.take_lines(0.5, WheelRoute::History), 1);
        assert_eq!(first.take_lines(0.75, WheelRoute::Mouse), 0);
        assert_eq!(first.take_lines(0.25, WheelRoute::Alternate), 0);
        assert_eq!(first.take_lines(0.75, WheelRoute::Alternate), 1);
        assert_eq!(first.take_lines(0.75, WheelRoute::History), 0);
        assert_eq!(first.take_lines(0.0, WheelRoute::Mouse), 0);
        assert_eq!(first.take_lines(0.25, WheelRoute::History), 0);
        first = WheelAccumulator::default();
        assert_eq!(first.take_lines(0.75, WheelRoute::History), 0);
    }

    #[test]
    fn wheel_routes_mouse_before_alternate_and_shift_to_history() {
        for mouse in [
            MouseMode::None,
            MouseMode::Normal,
            MouseMode::ButtonMotion,
            MouseMode::AnyMotion,
        ] {
            for alternate in [false, true] {
                assert_eq!(
                    WheelRoute::for_modes(mouse, alternate, true),
                    WheelRoute::History
                );
                let expected = if mouse != MouseMode::None {
                    WheelRoute::Mouse
                } else if alternate {
                    WheelRoute::Alternate
                } else {
                    WheelRoute::History
                };
                assert_eq!(WheelRoute::for_modes(mouse, alternate, false), expected);
            }
        }
    }

    #[test]
    fn alternate_wheel_repeats_normal_and_application_arrows() {
        assert_eq!(encode_alternate_scroll(true, 2, false), b"\x1b[A\x1b[A");
        assert_eq!(encode_alternate_scroll(false, 2, false), b"\x1b[B\x1b[B");
        assert_eq!(encode_alternate_scroll(true, 2, true), b"\x1bOA\x1bOA");
        assert_eq!(encode_alternate_scroll(false, 2, true), b"\x1bOB\x1bOB");
        assert!(encode_alternate_scroll(true, 0, true).is_empty());
    }

    #[test]
    fn mouse_wheel_repeats_complete_reports_with_modifiers() {
        let modifiers = Modifiers {
            alt: true,
            control: true,
            ..Default::default()
        };
        assert_eq!(
            encode_mouse_wheel(true, 2, 2, 4, modifiers),
            b"\x1b[<88;3;5M\x1b[<88;3;5M"
        );
        assert_eq!(
            encode_mouse_wheel(false, 3, 0, 0, Modifiers::default()),
            b"\x1b[<65;1;1M\x1b[<65;1;1M\x1b[<65;1;1M"
        );
        assert!(encode_mouse_wheel(true, 0, 0, 0, modifiers).is_empty());
    }

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
        assert_eq!(encode_keystroke(&ctrl_c, false, None, 0), Some(vec![3]));
        let space = Key {
            key: "space",
            ..Default::default()
        };
        assert_eq!(
            encode_keystroke(&space, false, None, 0),
            Some(b" ".to_vec())
        );
        let up = Key {
            key: "up",
            ..Default::default()
        };
        assert_eq!(
            encode_keystroke(&up, false, None, 0),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_keystroke(&up, true, None, 0),
            Some(b"\x1bOA".to_vec())
        );
        let application_home = Key {
            key: "home",
            ..Default::default()
        };
        assert_eq!(
            encode_keystroke(&application_home, true, None, 0),
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
            encode_keystroke(&modified_up, true, None, 0),
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
        assert_eq!(encode_keystroke(&ctrl_space, false, None, 0), Some(vec![0]));
        let ctrl_backspace = Key {
            modifiers: Modifiers {
                control: true,
                ..Default::default()
            },
            key: "backspace",
            key_char: None,
        };
        assert_eq!(
            encode_keystroke(&ctrl_backspace, false, None, 0),
            Some(vec![0x17])
        );
        assert_eq!(
            encode_keystroke(&space, false, Some(KeypadKey::Digit(7)), 0),
            Some(b"\x1bOw".to_vec())
        );
    }
    #[test]
    fn ctrl_enter_requires_negotiated_keyboard_mode() {
        let key = Key {
            modifiers: Modifiers {
                control: true,
                ..Default::default()
            },
            key: "enter",
            ..Default::default()
        };
        assert_eq!(encode_keystroke(&key, false, None, 0), Some(b"\r".to_vec()));
        assert_eq!(
            encode_keystroke(&key, false, None, 1),
            Some(b"\x1b[13;5u".to_vec())
        );
        assert_eq!(encode_keystroke(&key, false, None, 2), Some(b"\r".to_vec()));
        let alt_ctrl_enter = Key {
            modifiers: Modifiers {
                alt: true,
                ..key.modifiers
            },
            ..key
        };
        assert_eq!(
            encode_keystroke(&alt_ctrl_enter, false, None, 0),
            Some(b"\x1b\r".to_vec())
        );
        assert_eq!(
            encode_keystroke(&alt_ctrl_enter, false, None, 1),
            Some(b"\x1b[13;7u".to_vec())
        );
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
