use alacritty_terminal::term::TermMode;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn encode_key(key: KeyEvent, mode: TermMode) -> Option<Vec<u8>> {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
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
        _ => return None,
    };
    Some(bytes)
}

pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let body = text.replace("\x1b[201~", "");
        [b"\x1b[200~".as_slice(), body.as_bytes(), b"\x1b[201~"].concat()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

pub fn encode_wheel(up: bool, col: u16, row: u16, mode: TermMode) -> Option<Vec<u8>> {
    if mode.intersects(TermMode::MOUSE_MODE) {
        let button: u8 = if up { 64 } else { 65 };
        if mode.contains(TermMode::SGR_MOUSE) {
            Some(format!("\x1b[<{button};{};{}M", col + 1, row + 1).into_bytes())
        } else {
            let coord = |v: u16| u8::try_from(v + 33).unwrap_or(u8::MAX);
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

pub fn is_detach(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('5'))
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

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn enc(code: KeyCode, mods: KeyModifiers) -> Vec<u8> {
        encode_key(key(code, mods), TermMode::empty()).expect("encodable")
    }

    #[test]
    fn plain_chars_are_utf8() {
        assert_eq!(enc(KeyCode::Char('a'), KeyModifiers::NONE), b"a");
        assert_eq!(enc(KeyCode::Char('A'), KeyModifiers::SHIFT), b"A");
        assert_eq!(enc(KeyCode::Char('é'), KeyModifiers::NONE), "é".as_bytes());
    }

    #[test]
    fn ctrl_letters_are_control_bytes() {
        assert_eq!(enc(KeyCode::Char('c'), KeyModifiers::CONTROL), [0x03]);
        assert_eq!(enc(KeyCode::Char('r'), KeyModifiers::CONTROL), [0x12]);
        assert_eq!(enc(KeyCode::Char(' '), KeyModifiers::CONTROL), [0x00]);
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(enc(KeyCode::Char('b'), KeyModifiers::ALT), b"\x1bb");
        assert_eq!(enc(KeyCode::Backspace, KeyModifiers::ALT), b"\x1b\x7f");
    }

    #[test]
    fn plain_enter_is_cr() {
        assert_eq!(enc(KeyCode::Enter, KeyModifiers::NONE), b"\r");
    }

    #[test]
    fn alt_enter_is_esc_cr() {
        assert_eq!(enc(KeyCode::Enter, KeyModifiers::ALT), b"\x1b\r");
        assert_eq!(enc(KeyCode::Enter, KeyModifiers::SHIFT), b"\x1b\r");
    }

    #[test]
    fn editing_keys() {
        assert_eq!(enc(KeyCode::Tab, KeyModifiers::NONE), b"\t");
        assert_eq!(enc(KeyCode::BackTab, KeyModifiers::SHIFT), b"\x1b[Z");
        assert_eq!(enc(KeyCode::Backspace, KeyModifiers::NONE), [0x7f]);
        assert_eq!(enc(KeyCode::Esc, KeyModifiers::NONE), [0x1b]);
        assert_eq!(enc(KeyCode::Delete, KeyModifiers::NONE), b"\x1b[3~");
        assert_eq!(enc(KeyCode::PageUp, KeyModifiers::NONE), b"\x1b[5~");
    }

    #[test]
    fn arrows_follow_cursor_mode() {
        assert_eq!(enc(KeyCode::Up, KeyModifiers::NONE), b"\x1b[A");
        let app = encode_key(key(KeyCode::Up, KeyModifiers::NONE), TermMode::APP_CURSOR);
        assert_eq!(app.as_deref(), Some(b"\x1bOA".as_slice()));
    }

    #[test]
    fn modified_arrows_use_csi_params() {
        assert_eq!(enc(KeyCode::Left, KeyModifiers::CONTROL), b"\x1b[1;5D");
        assert_eq!(enc(KeyCode::Right, KeyModifiers::SHIFT), b"\x1b[1;2C");
        assert_eq!(enc(KeyCode::Delete, KeyModifiers::ALT), b"\x1b[3;3~");
    }

    #[test]
    fn function_keys() {
        assert_eq!(enc(KeyCode::F(1), KeyModifiers::NONE), b"\x1bOP");
        assert_eq!(enc(KeyCode::F(5), KeyModifiers::NONE), b"\x1b[15~");
        assert_eq!(enc(KeyCode::F(12), KeyModifiers::NONE), b"\x1b[24~");
        assert_eq!(
            encode_key(key(KeyCode::F(13), KeyModifiers::NONE), TermMode::empty()),
            None
        );
    }

    #[test]
    fn paste_without_brackets_maps_newlines() {
        assert_eq!(encode_paste("a\nb\r\nc", TermMode::empty()), b"a\rb\rc");
    }

    #[test]
    fn bracketed_paste_wraps_text() {
        assert_eq!(
            encode_paste("hi\n", TermMode::BRACKETED_PASTE),
            b"\x1b[200~hi\n\x1b[201~"
        );
    }

    #[test]
    fn bracketed_paste_strips_end_marker() {
        assert_eq!(
            encode_paste("a\x1b[201~b", TermMode::BRACKETED_PASTE),
            b"\x1b[200~ab\x1b[201~"
        );
    }

    #[test]
    fn wheel_scrolls_scrollback_without_mouse_mode() {
        assert_eq!(encode_wheel(true, 0, 0, TermMode::empty()), None);
    }

    #[test]
    fn wheel_reports_sgr_when_requested() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(
            encode_wheel(true, 4, 9, mode).as_deref(),
            Some(b"\x1b[<64;5;10M".as_slice())
        );
        assert_eq!(
            encode_wheel(false, 0, 0, mode).as_deref(),
            Some(b"\x1b[<65;1;1M".as_slice())
        );
    }

    #[test]
    fn wheel_sends_arrows_on_alternate_screen() {
        let mode = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        assert_eq!(
            encode_wheel(true, 0, 0, mode).as_deref(),
            Some(b"\x1b[A\x1b[A\x1b[A".as_slice())
        );
    }

    #[test]
    fn detach_matches_ctrl_bracket() {
        assert!(is_detach(&key(KeyCode::Char(']'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn detach_matches_legacy_ctrl_5() {
        assert!(is_detach(&key(KeyCode::Char('5'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn detach_ignores_plain_bracket() {
        assert!(!is_detach(&key(KeyCode::Char(']'), KeyModifiers::NONE)));
    }
}
