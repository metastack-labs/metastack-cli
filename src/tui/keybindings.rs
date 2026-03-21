use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavigationDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeybindingPolicy {
    vim_mode: bool,
}

impl KeybindingPolicy {
    pub(crate) fn new(vim_mode: bool) -> Self {
        Self { vim_mode }
    }

    pub(crate) fn vim_mode_enabled(self) -> bool {
        self.vim_mode
    }

    pub(crate) fn navigation_direction(self, key: KeyEvent) -> Option<NavigationDirection> {
        match key.code {
            KeyCode::Up => Some(NavigationDirection::Up),
            KeyCode::Down => Some(NavigationDirection::Down),
            KeyCode::Left => Some(NavigationDirection::Left),
            KeyCode::Right => Some(NavigationDirection::Right),
            KeyCode::Char('k') if self.vim_mode && plain_char(key) => Some(NavigationDirection::Up),
            KeyCode::Char('j') if self.vim_mode && plain_char(key) => {
                Some(NavigationDirection::Down)
            }
            KeyCode::Char('h') if self.vim_mode && plain_char(key) => {
                Some(NavigationDirection::Left)
            }
            KeyCode::Char('l') if self.vim_mode && plain_char(key) => {
                Some(NavigationDirection::Right)
            }
            _ => None,
        }
    }

    pub(crate) fn vertical_delta(self, key: KeyEvent) -> Option<isize> {
        match self.navigation_direction(key) {
            Some(NavigationDirection::Up) => Some(-1),
            Some(NavigationDirection::Down) => Some(1),
            _ => None,
        }
    }

    pub(crate) fn horizontal_delta(self, key: KeyEvent) -> Option<isize> {
        match self.navigation_direction(key) {
            Some(NavigationDirection::Left) => Some(-1),
            Some(NavigationDirection::Right) => Some(1),
            _ => None,
        }
    }
}

fn plain_char(key: KeyEvent) -> bool {
    key.modifiers.is_empty() || key.modifiers == KeyModifiers::NONE
}

#[cfg(test)]
mod tests {
    use super::{KeybindingPolicy, NavigationDirection};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn vim_navigation_aliases_are_disabled_by_default() {
        let policy = KeybindingPolicy::new(false);

        assert_eq!(
            policy.navigation_direction(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            policy.navigation_direction(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Some(NavigationDirection::Up)
        );
    }

    #[test]
    fn vim_navigation_aliases_map_to_arrow_directions_when_enabled() {
        let policy = KeybindingPolicy::new(true);

        assert_eq!(
            policy.navigation_direction(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            Some(NavigationDirection::Up)
        );
        assert_eq!(
            policy.navigation_direction(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(NavigationDirection::Down)
        );
        assert_eq!(
            policy.navigation_direction(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE)),
            Some(NavigationDirection::Left)
        );
        assert_eq!(
            policy.navigation_direction(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)),
            Some(NavigationDirection::Right)
        );
    }

    #[test]
    fn modified_vim_keys_do_not_trigger_navigation_aliases() {
        let policy = KeybindingPolicy::new(true);

        assert_eq!(
            policy.navigation_direction(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            None
        );
    }
}
