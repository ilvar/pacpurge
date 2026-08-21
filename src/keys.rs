//! The keymap: terminal key events to [`Intent`]s.
//!
//! Kept apart from [`crate::app`] so that the state machine only ever sees
//! intents it knows how to handle, and so that adding a binding cannot
//! accidentally change behaviour. `KeyCode` has dozens of variants this
//! program has no opinion about, which is why this is the one place a
//! catch-all match arm is the right answer.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::Intent;
use crate::filter::{SortKey, Toggle};

/// Translate a key press into an intent.
///
/// `typing` is true while the search field has focus, where printable
/// characters are text rather than commands.
pub fn intent(key: KeyEvent, typing: bool) -> Option<Intent> {
    if key.kind == KeyEventKind::Release {
        return None;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Some(Intent::Quit),
            KeyCode::Char('d') => Some(Intent::PageDown),
            KeyCode::Char('u') => Some(Intent::PageUp),
            _ => None,
        };
    }

    if typing {
        return match key.code {
            KeyCode::Esc => Some(Intent::SearchCancel),
            KeyCode::Enter => Some(Intent::SearchCommit),
            KeyCode::Backspace => Some(Intent::SearchBackspace),
            KeyCode::Char(character) => Some(Intent::SearchInput(character)),
            KeyCode::Up => Some(Intent::Up),
            KeyCode::Down => Some(Intent::Down),
            _ => None,
        };
    }

    match key.code {
        KeyCode::Char('q') => Some(Intent::Quit),
        KeyCode::Esc => Some(Intent::Cancel),
        KeyCode::Tab | KeyCode::Right => Some(Intent::NextTab),
        KeyCode::BackTab | KeyCode::Left => Some(Intent::PrevTab),

        KeyCode::Char('j') | KeyCode::Down => Some(Intent::Down),
        KeyCode::Char('k') | KeyCode::Up => Some(Intent::Up),
        KeyCode::PageDown => Some(Intent::PageDown),
        KeyCode::PageUp => Some(Intent::PageUp),
        KeyCode::Char('g') | KeyCode::Home => Some(Intent::First),
        KeyCode::Char('G') | KeyCode::End => Some(Intent::Last),

        KeyCode::Char(' ') => Some(Intent::ToggleSelect),
        KeyCode::Char('P') => Some(Intent::ForceSelect),
        KeyCode::Char('c') => Some(Intent::ClearSelection),
        KeyCode::Char('/') => Some(Intent::StartSearch),

        KeyCode::Char('o') => Some(Intent::Filter(Toggle::Orphans)),
        KeyCode::Char('a') => Some(Intent::Filter(Toggle::Foreign)),
        KeyCode::Char('e') => Some(Intent::Filter(Toggle::Explicit)),
        KeyCode::Char('n') => Some(Intent::Filter(Toggle::NeverUsed)),
        KeyCode::Char('u') => Some(Intent::Filter(Toggle::Stale)),
        KeyCode::Char('p') => Some(Intent::ToggleProtected),
        KeyCode::Char('D') => Some(Intent::ToggleDescriptions),

        KeyCode::Char('s') => Some(Intent::CycleSort),
        KeyCode::Char('S') => Some(Intent::ReverseSort),
        KeyCode::Char('1') => Some(Intent::SortBy(SortKey::Name)),
        KeyCode::Char('2') => Some(Intent::SortBy(SortKey::Size)),
        KeyCode::Char('3') => Some(Intent::SortBy(SortKey::Reclaimable)),
        KeyCode::Char('4') => Some(Intent::SortBy(SortKey::LastUsed)),
        KeyCode::Char('5') => Some(Intent::SortBy(SortKey::Installed)),
        KeyCode::Char('6') => Some(Intent::SortBy(SortKey::RequiredBy)),

        KeyCode::Enter | KeyCode::Char('x') => Some(Intent::Review),
        KeyCode::Char('y') => Some(Intent::Accept),
        KeyCode::Char('r') => Some(Intent::Rescan),
        KeyCode::Char('?') => Some(Intent::Help),

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    use super::intent;
    use crate::app::Intent;
    use crate::filter::Toggle;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn printable_keys_are_commands_when_not_typing() {
        assert_eq!(
            intent(press(KeyCode::Char('o')), false),
            Some(Intent::Filter(Toggle::Orphans))
        );
        assert_eq!(intent(press(KeyCode::Char('q')), false), Some(Intent::Quit));
    }

    #[test]
    fn printable_keys_are_text_when_typing() {
        assert_eq!(
            intent(press(KeyCode::Char('o')), true),
            Some(Intent::SearchInput('o'))
        );
        assert_eq!(
            intent(press(KeyCode::Char('q')), true),
            Some(Intent::SearchInput('q'))
        );
    }

    #[test]
    fn arrows_still_navigate_while_typing() {
        assert_eq!(intent(press(KeyCode::Down), true), Some(Intent::Down));
        assert_eq!(
            intent(press(KeyCode::Esc), true),
            Some(Intent::SearchCancel)
        );
    }

    #[test]
    fn control_c_always_quits() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(intent(key, false), Some(Intent::Quit));
        assert_eq!(intent(key, true), Some(Intent::Quit));
    }

    #[test]
    fn key_releases_are_ignored() {
        // Windows terminals and some Linux ones report both press and release;
        // acting on both would double every keystroke.
        let mut key = press(KeyCode::Char('q'));
        key.kind = KeyEventKind::Release;
        assert_eq!(intent(key, false), None);
    }

    #[test]
    fn case_distinguishes_related_bindings() {
        // `p` hides protected packages; `P` marks one anyway; `D` widens the
        // search. Getting these confused would be destructive, so they are
        // pinned.
        assert_eq!(
            intent(press(KeyCode::Char('p')), false),
            Some(Intent::ToggleProtected)
        );
        assert_eq!(
            intent(press(KeyCode::Char('P')), false),
            Some(Intent::ForceSelect)
        );
        assert_eq!(
            intent(press(KeyCode::Char('D')), false),
            Some(Intent::ToggleDescriptions)
        );
    }

    #[test]
    fn unbound_keys_produce_nothing() {
        assert_eq!(intent(press(KeyCode::F(7)), false), None);
        assert_eq!(intent(press(KeyCode::Insert), true), None);
    }
}
