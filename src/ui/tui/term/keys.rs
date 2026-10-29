//! Клавиши crossterm → [`Key`] просмотрщика.
//!
//! Наружу: [`is_quit`], [`map`].

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::desk::Key;

/// Ctrl+C — выход из любого места, даже из строки ввода.
pub(super) fn is_quit(k: &KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c')
}

pub(super) fn map(k: &KeyEvent) -> Option<Key> {
    Some(match k.code {
        // F1..F4 кодируем непечатаемыми знаками — так их же передаёт окно
        // ([`crate::ui::App::keys`]): F1 — справка, F2 — вид TUI, F3 — экран
        // агента, F4 — редактор Lua-кода.
        KeyCode::F(1) => Key::Char('\u{F1}'),
        KeyCode::F(2) => Key::Char('\u{F2}'),
        KeyCode::F(3) => Key::Char('\u{F3}'),
        KeyCode::F(4) => Key::Char('\u{F4}'),
        // F5 — список линков resolve.
        KeyCode::F(5) => Key::Char('\u{F5}'),
        KeyCode::Char(c) if k.modifiers.contains(KeyModifiers::CONTROL) => Key::Ctrl(c),
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn ev(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: m,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    /// Клавиши терминала те же, что у окна; Ctrl+C — выход, Ctrl+буква — команда.
    #[test]
    fn terminal_keys_map_to_desk_keys() {
        let n = KeyModifiers::NONE;
        assert_eq!(map(&ev(KeyCode::Char('f'), n)), Some(Key::Char('f')));
        assert_eq!(
            map(&ev(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            Some(Key::Ctrl('x'))
        );
        assert_eq!(map(&ev(KeyCode::F(2), n)), Some(Key::Char('\u{F2}')));
        assert_eq!(map(&ev(KeyCode::F(3), n)), Some(Key::Char('\u{F3}')));
        assert_eq!(map(&ev(KeyCode::F(4), n)), Some(Key::Char('\u{F4}')));
        let pairs = [
            (KeyCode::Enter, Key::Enter),
            (KeyCode::Esc, Key::Esc),
            (KeyCode::Tab, Key::Tab),
            (KeyCode::BackTab, Key::BackTab),
            (KeyCode::Backspace, Key::Backspace),
            (KeyCode::Delete, Key::Delete),
            (KeyCode::Up, Key::Up),
            (KeyCode::Down, Key::Down),
            (KeyCode::Left, Key::Left),
            (KeyCode::Right, Key::Right),
            (KeyCode::PageUp, Key::PageUp),
            (KeyCode::PageDown, Key::PageDown),
            (KeyCode::Home, Key::Home),
            (KeyCode::End, Key::End),
        ];
        for (c, k) in pairs {
            assert_eq!(map(&ev(c, n)), Some(k));
        }
        assert_eq!(map(&ev(KeyCode::F(5), n)), Some(Key::Char('\u{F5}')));
        assert_eq!(map(&ev(KeyCode::F(6), n)), None);
        assert!(is_quit(&ev(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        assert!(!is_quit(&ev(KeyCode::Char('c'), n)));
    }
}
