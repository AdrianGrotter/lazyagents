use alacritty_terminal::term::TermMode;
use protocol::{InputEvent, Key, KeyCode};

pub fn encode(event: &InputEvent, mode: TermMode) -> Option<Vec<u8>> {
    match event {
        InputEvent::Key(key) => encode_key(*key, mode),
        InputEvent::Paste(text) => Some(encode_paste(text, mode)),
        InputEvent::Wheel { up, col, row } => encode_wheel(*up, *col, *row, mode),
    }
}

fn encode_key(key: Key, mode: TermMode) -> Option<Vec<u8>> {
    let shift = key.mods.shift;
    let alt = key.mods.alt;
    let ctrl = key.mods.ctrl;
    let param = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let app_cursor = mode.contains(TermMode::APP_CURSOR);

    let bytes = match key.code {
        KeyCode::Char(c) => encode_char(c, ctrl, alt),
        KeyCode::Enter if shift || alt => b"\x1b\r".to_vec(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab if shift => b"\x1b[Z".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace if ctrl => vec![0x08],
        KeyCode::Backspace if alt => b"\x1b\x7f".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => cursor_key(b'A', param, app_cursor),
        KeyCode::Down => cursor_key(b'B', param, app_cursor),
        KeyCode::Right => cursor_key(b'C', param, app_cursor),
        KeyCode::Left => cursor_key(b'D', param, app_cursor),
        KeyCode::Home => cursor_key(b'H', param, app_cursor),
        KeyCode::End => cursor_key(b'F', param, app_cursor),
        KeyCode::Insert => tilde_key(2, param),
        KeyCode::Delete => tilde_key(3, param),
        KeyCode::PageUp => tilde_key(5, param),
        KeyCode::PageDown => tilde_key(6, param),
        KeyCode::F(n) => function_key(n, param)?,
    };
    Some(bytes)
}

fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        const END: &str = "\x1b[201~";
        let mut body = text.to_owned();
        while body.contains(END) {
            body = body.replace(END, "");
        }
        [b"\x1b[200~".as_slice(), body.as_bytes(), END.as_bytes()].concat()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

fn encode_wheel(up: bool, col: u16, row: u16, mode: TermMode) -> Option<Vec<u8>> {
    if mode.intersects(TermMode::MOUSE_MODE) {
        let button: u8 = if up { 64 } else { 65 };
        if mode.contains(TermMode::SGR_MOUSE) {
            Some(
                format!(
                    "\x1b[<{button};{};{}M",
                    col.saturating_add(1),
                    row.saturating_add(1)
                )
                .into_bytes(),
            )
        } else {
            let coord = |v: u16| u8::try_from(v.saturating_add(33)).unwrap_or(u8::MAX);
            Some(vec![0x1b, b'[', b'M', 32 + button, coord(col), coord(row)])
        }
    } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
        let arrow = cursor_key(
            if up { b'A' } else { b'B' },
            1,
            mode.contains(TermMode::APP_CURSOR),
        );
        Some(arrow.repeat(3))
    } else {
        None
    }
}

fn encode_char(c: char, ctrl: bool, alt: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(5);
    if alt {
        out.push(0x1b);
    }
    match ctrl.then(|| ctrl_byte(c)).flatten() {
        Some(byte) => out.push(byte),
        None => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
    }
    out
}

fn ctrl_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        ' ' | '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

fn cursor_key(final_byte: u8, param: u8, app_cursor: bool) -> Vec<u8> {
    if param > 1 {
        format!("\x1b[1;{param}{}", final_byte as char).into_bytes()
    } else if app_cursor {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

fn tilde_key(code: u8, param: u8) -> Vec<u8> {
    if param > 1 {
        format!("\x1b[{code};{param}~").into_bytes()
    } else {
        format!("\x1b[{code}~").into_bytes()
    }
}

fn function_key(n: u8, param: u8) -> Option<Vec<u8>> {
    match n {
        1..=4 => {
            let final_byte = b"PQRS"[usize::from(n - 1)];
            Some(if param > 1 {
                format!("\x1b[1;{param}{}", final_byte as char).into_bytes()
            } else {
                vec![0x1b, b'O', final_byte]
            })
        }
        5..=12 => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)];
            Some(tilde_key(code, param))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Modifiers;

    fn key(code: KeyCode, mods: Modifiers) -> Vec<u8> {
        encode(&InputEvent::Key(Key::new(code, mods)), TermMode::empty()).expect("encodable")
    }

    fn paste(text: &str, mode: TermMode) -> Option<Vec<u8>> {
        encode(&InputEvent::Paste(text.to_string()), mode)
    }

    #[test]
    fn plain_chars_are_utf8() {
        assert_eq!(key(KeyCode::Char('a'), Modifiers::NONE), b"a");
        assert_eq!(key(KeyCode::Char('A'), Modifiers::SHIFT), b"A");
        assert_eq!(key(KeyCode::Char('é'), Modifiers::NONE), "é".as_bytes());
    }

    #[test]
    fn ctrl_letters_are_control_bytes() {
        assert_eq!(key(KeyCode::Char('c'), Modifiers::CTRL), [0x03]);
        assert_eq!(key(KeyCode::Char('r'), Modifiers::CTRL), [0x12]);
        assert_eq!(key(KeyCode::Char(' '), Modifiers::CTRL), [0x00]);
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(key(KeyCode::Char('b'), Modifiers::ALT), b"\x1bb");
        assert_eq!(key(KeyCode::Backspace, Modifiers::ALT), b"\x1b\x7f");
    }

    #[test]
    fn plain_enter_is_cr() {
        assert_eq!(key(KeyCode::Enter, Modifiers::NONE), b"\r");
    }

    #[test]
    fn alt_enter_is_esc_cr() {
        assert_eq!(key(KeyCode::Enter, Modifiers::ALT), b"\x1b\r");
        assert_eq!(key(KeyCode::Enter, Modifiers::SHIFT), b"\x1b\r");
    }

    #[test]
    fn editing_keys() {
        assert_eq!(key(KeyCode::Tab, Modifiers::NONE), b"\t");
        assert_eq!(key(KeyCode::BackTab, Modifiers::SHIFT), b"\x1b[Z");
        assert_eq!(key(KeyCode::Backspace, Modifiers::NONE), [0x7f]);
        assert_eq!(key(KeyCode::Esc, Modifiers::NONE), [0x1b]);
        assert_eq!(key(KeyCode::Delete, Modifiers::NONE), b"\x1b[3~");
        assert_eq!(key(KeyCode::PageUp, Modifiers::NONE), b"\x1b[5~");
    }

    #[test]
    fn arrows_follow_cursor_mode() {
        assert_eq!(key(KeyCode::Up, Modifiers::NONE), b"\x1b[A");
        let app = encode(
            &InputEvent::Key(Key::new(KeyCode::Up, Modifiers::NONE)),
            TermMode::APP_CURSOR,
        );
        assert_eq!(app.as_deref(), Some(b"\x1bOA".as_slice()));
    }

    #[test]
    fn modified_arrows_use_csi_params() {
        assert_eq!(key(KeyCode::Left, Modifiers::CTRL), b"\x1b[1;5D");
        assert_eq!(key(KeyCode::Right, Modifiers::SHIFT), b"\x1b[1;2C");
        assert_eq!(key(KeyCode::Delete, Modifiers::ALT), b"\x1b[3;3~");
    }

    #[test]
    fn function_keys() {
        assert_eq!(key(KeyCode::F(1), Modifiers::NONE), b"\x1bOP");
        assert_eq!(key(KeyCode::F(5), Modifiers::NONE), b"\x1b[15~");
        assert_eq!(key(KeyCode::F(12), Modifiers::NONE), b"\x1b[24~");
        assert_eq!(
            encode(
                &InputEvent::Key(Key::new(KeyCode::F(13), Modifiers::NONE)),
                TermMode::empty()
            ),
            None
        );
    }

    #[test]
    fn paste_without_brackets_maps_newlines() {
        assert_eq!(
            paste("a\nb\r\nc", TermMode::empty()).as_deref(),
            Some(b"a\rb\rc".as_slice())
        );
    }

    #[test]
    fn bracketed_paste_wraps_text() {
        assert_eq!(
            paste("hi\n", TermMode::BRACKETED_PASTE).as_deref(),
            Some(b"\x1b[200~hi\n\x1b[201~".as_slice())
        );
    }

    #[test]
    fn bracketed_paste_strips_end_marker() {
        assert_eq!(
            paste("a\x1b[201~b", TermMode::BRACKETED_PASTE).as_deref(),
            Some(b"\x1b[200~ab\x1b[201~".as_slice())
        );
    }

    #[test]
    fn bracketed_paste_strips_nested_end_marker() {
        assert_eq!(
            paste("a\x1b[20\x1b[201~1~b", TermMode::BRACKETED_PASTE).as_deref(),
            Some(b"\x1b[200~ab\x1b[201~".as_slice())
        );
    }

    #[test]
    fn wheel_scrolls_scrollback_without_mouse_mode() {
        assert_eq!(
            encode(
                &InputEvent::Wheel {
                    up: true,
                    col: 0,
                    row: 0
                },
                TermMode::empty()
            ),
            None
        );
    }

    #[test]
    fn wheel_reports_sgr_when_requested() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(
            encode(
                &InputEvent::Wheel {
                    up: true,
                    col: 4,
                    row: 9
                },
                mode
            )
            .as_deref(),
            Some(b"\x1b[<64;5;10M".as_slice())
        );
        assert_eq!(
            encode(
                &InputEvent::Wheel {
                    up: false,
                    col: 0,
                    row: 0
                },
                mode
            )
            .as_deref(),
            Some(b"\x1b[<65;1;1M".as_slice())
        );
    }

    #[test]
    fn wheel_sends_arrows_on_alternate_screen() {
        let mode = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        assert_eq!(
            encode(
                &InputEvent::Wheel {
                    up: true,
                    col: 0,
                    row: 0
                },
                mode
            )
            .as_deref(),
            Some(b"\x1b[A\x1b[A\x1b[A".as_slice())
        );
    }

    #[test]
    fn navigation_keys() {
        assert_eq!(key(KeyCode::Home, Modifiers::NONE), b"\x1b[H");
        assert_eq!(key(KeyCode::End, Modifiers::NONE), b"\x1b[F");
        assert_eq!(key(KeyCode::Insert, Modifiers::NONE), b"\x1b[2~");
        assert_eq!(key(KeyCode::PageDown, Modifiers::NONE), b"\x1b[6~");
        let app_home = encode(
            &InputEvent::Key(Key::new(KeyCode::Home, Modifiers::NONE)),
            TermMode::APP_CURSOR,
        );
        assert_eq!(app_home.as_deref(), Some(b"\x1bOH".as_slice()));
    }

    #[test]
    fn every_function_key() {
        let expected: [&[u8]; 12] = [
            b"\x1bOP",
            b"\x1bOQ",
            b"\x1bOR",
            b"\x1bOS",
            b"\x1b[15~",
            b"\x1b[17~",
            b"\x1b[18~",
            b"\x1b[19~",
            b"\x1b[20~",
            b"\x1b[21~",
            b"\x1b[23~",
            b"\x1b[24~",
        ];
        for (n, bytes) in (1..=12).zip(expected) {
            assert_eq!(key(KeyCode::F(n), Modifiers::NONE), bytes, "F{n}");
        }
        assert_eq!(key(KeyCode::F(2), Modifiers::SHIFT), b"\x1b[1;2Q");
        assert_eq!(key(KeyCode::F(5), Modifiers::CTRL), b"\x1b[15;5~");
    }

    #[test]
    fn ctrl_backspace_is_bs() {
        assert_eq!(key(KeyCode::Backspace, Modifiers::CTRL), [0x08]);
    }

    #[test]
    fn x10_mouse_wheel() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        let bytes = encode(
            &InputEvent::Wheel {
                up: true,
                col: 0,
                row: 1,
            },
            mode,
        )
        .expect("bytes");
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 96, 33, 34]);
    }

    #[test]
    fn x10_mouse_saturates_far_coordinates() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        let bytes = encode(
            &InputEvent::Wheel {
                up: false,
                col: u16::MAX,
                row: 500,
            },
            mode,
        )
        .expect("bytes");
        assert_eq!(bytes[4..], [u8::MAX, u8::MAX]);
    }

    #[test]
    fn sgr_mouse_saturates_far_coordinates() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let bytes = encode(
            &InputEvent::Wheel {
                up: true,
                col: u16::MAX,
                row: 0,
            },
            mode,
        )
        .expect("bytes");
        assert_eq!(bytes, format!("\x1b[<64;{};1M", u16::MAX).into_bytes());
    }

    #[test]
    fn alternate_scroll_respects_app_cursor() {
        let mode = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL | TermMode::APP_CURSOR;
        let bytes = encode(
            &InputEvent::Wheel {
                up: false,
                col: 0,
                row: 0,
            },
            mode,
        )
        .expect("bytes");
        assert_eq!(bytes, b"\x1bOB\x1bOB\x1bOB");
    }
}
