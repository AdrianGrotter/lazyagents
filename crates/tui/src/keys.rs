use protocol::Key;
use ratatui::crossterm::event::KeyEvent;

pub fn to_protocol(_key: &KeyEvent) -> Option<Key> {
    None
}

pub fn is_detach(_key: &KeyEvent) -> bool {
    false
}

pub fn cycle_step(_key: &KeyEvent) -> Option<i32> {
    None
}
