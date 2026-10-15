use protocol::{Key, KeyCode as Code, Modifiers};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn to_protocol(key: &KeyEvent) -> Option<Key> {
    let code = match key.code {
        KeyCode::Char(c) => Code::Char(c),
        KeyCode::Enter => Code::Enter,
        KeyCode::Tab => Code::Tab,
        KeyCode::BackTab => Code::BackTab,
        KeyCode::Backspace => Code::Backspace,
        KeyCode::Esc => Code::Esc,
        KeyCode::Up => Code::Up,
        KeyCode::Down => Code::Down,
        KeyCode::Left => Code::Left,
        KeyCode::Right => Code::Right,
        KeyCode::Home => Code::Home,
        KeyCode::End => Code::End,
        KeyCode::Insert => Code::Insert,
        KeyCode::Delete => Code::Delete,
        KeyCode::PageUp => Code::PageUp,
        KeyCode::PageDown => Code::PageDown,
        KeyCode::F(n) => Code::F(n),
        _ => return None,
    };
    let mods = Modifiers {
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
    };
    Some(Key::new(code, mods))
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{KeyCode as Code, Modifiers};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn keys_map_with_modifiers() {
        assert_eq!(
            to_protocol(&ev(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(Key::new(Code::Char('a'), Modifiers::NONE))
        );
        assert_eq!(
            to_protocol(&ev(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Key::new(Code::Char('c'), Modifiers::CTRL))
        );
        assert_eq!(
            to_protocol(&ev(KeyCode::Enter, KeyModifiers::ALT)),
            Some(Key::new(Code::Enter, Modifiers::ALT))
        );
        assert_eq!(
            to_protocol(&ev(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Some(Key::new(Code::BackTab, Modifiers::SHIFT))
        );
        assert_eq!(
            to_protocol(&ev(KeyCode::F(5), KeyModifiers::NONE)),
            Some(Key::new(Code::F(5), Modifiers::NONE))
        );
        assert_eq!(
            to_protocol(&ev(KeyCode::PageDown, KeyModifiers::NONE)),
            Some(Key::new(Code::PageDown, Modifiers::NONE))
        );
        assert_eq!(
            to_protocol(&ev(KeyCode::CapsLock, KeyModifiers::NONE)),
            None
        );
    }
}
