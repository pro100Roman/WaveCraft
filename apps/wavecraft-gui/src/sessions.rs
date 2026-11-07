use crate::{CaptureTarget, LogicSessionSettings, ScopeSessionSettings, WaveformViewport};

pub(crate) const MAX_SESSION_TABS: usize = 10;
pub(crate) const SESSION_NAME_MAX_CHARS: usize = 48;

struct SessionTab<T, S> {
    name: String,
    capture: Option<T>,
    settings: S,
}

pub(crate) struct SessionTabs<T, S> {
    tabs: Vec<SessionTab<T, S>>,
    active: usize,
    next_default_number: usize,
}

impl<T, S> SessionTabs<T, S> {
    pub(crate) fn new(settings: S) -> Self {
        Self {
            tabs: vec![SessionTab {
                name: "Session 1".to_owned(),
                capture: None,
                settings,
            }],
            active: 0,
            next_default_number: 2,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.tabs.len()
    }

    pub(crate) fn active_index(&self) -> usize {
        self.active
    }

    pub(crate) fn names(&self) -> Vec<String> {
        self.tabs.iter().map(|tab| tab.name.clone()).collect()
    }

    pub(crate) fn name(&self, index: usize) -> Option<&str> {
        self.tabs.get(index).map(|tab| tab.name.as_str())
    }

    pub(crate) fn active_capture(&self) -> Option<&T> {
        self.tabs
            .get(self.active)
            .and_then(|tab| tab.capture.as_ref())
    }

    pub(crate) fn active_settings(&self) -> &S {
        &self.tabs[self.active].settings
    }

    pub(crate) fn replace_active_settings(&mut self, settings: S) {
        self.tabs[self.active].settings = settings;
    }

    pub(crate) fn replace_active(&mut self, value: T) {
        self.tabs[self.active].capture = Some(value);
    }

    pub(crate) fn clear_active(&mut self) {
        self.tabs[self.active].capture = None;
    }

    pub(crate) fn take_active(&mut self) -> Option<T> {
        self.tabs
            .get_mut(self.active)
            .and_then(|tab| tab.capture.take())
    }

    pub(crate) fn add(&mut self, settings: S) -> bool {
        if self.len() >= MAX_SESSION_TABS {
            return false;
        }
        let name = format!("Session {}", self.next_default_number);
        self.next_default_number += 1;
        self.tabs.push(SessionTab {
            name,
            capture: None,
            settings,
        });
        self.active = self.tabs.len() - 1;
        true
    }

    pub(crate) fn rename(&mut self, index: usize, name: &str) -> bool {
        let Some(tab) = self.tabs.get_mut(index) else {
            return false;
        };
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        tab.name = name.chars().take(SESSION_NAME_MAX_CHARS).collect();
        true
    }

    pub(crate) fn select(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return false;
        }
        self.active = index;
        true
    }

    pub(crate) fn close(&mut self, index: usize) -> bool {
        if self.len() <= 1 || index >= self.len() {
            return false;
        }
        self.tabs.remove(index);
        if index < self.active {
            self.active -= 1;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        true
    }

    pub(crate) fn reset(&mut self, settings: S) {
        self.tabs.clear();
        self.tabs.push(SessionTab {
            name: "Session 1".to_owned(),
            capture: None,
            settings,
        });
        self.active = 0;
        self.next_default_number = 2;
    }

    pub(crate) fn any(&self, predicate: impl Fn(&T) -> bool) -> bool {
        self.tabs
            .iter()
            .filter_map(|tab| tab.capture.as_ref())
            .any(predicate)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionTabAction {
    Select(usize),
    Add,
    Close(usize),
    BeginRename(usize),
}

pub(crate) struct SessionNameEditor {
    pub(crate) target: CaptureTarget,
    pub(crate) index: usize,
    pub(crate) draft: String,
    pub(crate) request_focus: bool,
}

#[derive(Clone, Default)]
pub(crate) struct CombinedSessionSettings {
    pub(crate) logic: LogicSessionSettings,
    pub(crate) scope: ScopeSessionSettings,
    pub(crate) logic_ratio: f32,
    pub(crate) viewport: WaveformViewport,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    #[test]
    fn session_tabs_preserve_each_tabs_capture_and_settings() {
        let mut sessions = SessionTabs::new(10_u64);
        sessions.replace_active("first capture");
        sessions.replace_active_settings(11);

        assert!(sessions.add(20));
        assert_eq!(sessions.active_index(), 1);
        assert_eq!(sessions.active_capture(), None);
        assert_eq!(*sessions.active_settings(), 20);
        sessions.replace_active("second capture");
        sessions.replace_active_settings(21);

        assert!(sessions.select(0));
        assert_eq!(sessions.active_capture(), Some(&"first capture"));
        assert_eq!(*sessions.active_settings(), 11);
        assert!(sessions.select(1));
        assert_eq!(sessions.active_capture(), Some(&"second capture"));
        assert_eq!(*sessions.active_settings(), 21);
    }

    #[test]
    fn session_tabs_are_limited_to_ten() {
        let mut sessions = SessionTabs::<(), ()>::new(());

        for _ in 1..MAX_SESSION_TABS {
            assert!(sessions.add(()));
        }

        assert_eq!(sessions.len(), MAX_SESSION_TABS);
        assert!(!sessions.add(()));
        assert_eq!(sessions.active_index(), MAX_SESSION_TABS - 1);
        assert!(!sessions.select(MAX_SESSION_TABS));
        assert_eq!(sessions.name(0), Some("Session 1"));
        assert_eq!(sessions.name(MAX_SESSION_TABS - 1), Some("Session 10"));
    }

    #[test]
    fn closing_sessions_discards_the_tab_and_keeps_active_session_valid() {
        let mut sessions = SessionTabs::new(10_u64);
        sessions.replace_active("first capture");
        assert!(sessions.add(20));
        sessions.replace_active("second capture");
        assert!(sessions.add(30));
        sessions.replace_active("third capture");

        assert!(sessions.close(0));
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions.names(), ["Session 2", "Session 3"]);
        assert_eq!(sessions.active_index(), 1);
        assert_eq!(*sessions.active_settings(), 30);
        assert_eq!(sessions.active_capture(), Some(&"third capture"));

        assert!(sessions.close(1));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions.active_index(), 0);
        assert_eq!(*sessions.active_settings(), 20);
        assert_eq!(sessions.active_capture(), Some(&"second capture"));
        assert!(!sessions.close(0));
        assert!(!sessions.close(1));

        assert!(sessions.add(40));
        assert_eq!(sessions.names(), ["Session 2", "Session 4"]);
    }

    #[test]
    fn closing_active_middle_session_selects_the_following_session() {
        let mut sessions = SessionTabs::<(), _>::new("first");
        assert!(sessions.add("second"));
        assert!(sessions.add("third"));
        assert!(sessions.select(1));

        assert!(sessions.close(1));

        assert_eq!(sessions.active_index(), 1);
        assert_eq!(*sessions.active_settings(), "third");
    }

    #[test]
    fn renamed_session_name_persists_across_selection_and_closing_other_tabs() {
        let mut sessions = SessionTabs::<(), _>::new("first");
        assert!(sessions.add("second"));
        assert!(sessions.add("third"));
        assert!(sessions.rename(1, "  SPI boot  "));
        assert!(!sessions.rename(1, "   "));

        assert!(sessions.select(0));
        assert!(sessions.select(1));
        assert_eq!(sessions.name(1), Some("SPI boot"));

        assert!(sessions.close(0));
        assert_eq!(sessions.names(), ["SPI boot", "Session 3"]);
        assert_eq!(sessions.name(0), Some("SPI boot"));
    }

    #[test]
    fn session_tabs_preserve_selected_device_ids() {
        let first = LogicSessionSettings {
            device_id: Some("usb-logic-a".to_owned()),
            ..LogicSessionSettings::default()
        };
        let mut sessions = SessionTabs::<(), _>::new(first);
        let second = LogicSessionSettings {
            device_id: Some("usb-logic-b".to_owned()),
            ..LogicSessionSettings::default()
        };

        assert!(sessions.add(second));
        assert_eq!(
            sessions.active_settings().device_id.as_deref(),
            Some("usb-logic-b")
        );
        assert!(sessions.select(0));
        assert_eq!(
            sessions.active_settings().device_id.as_deref(),
            Some("usb-logic-a")
        );
    }
}
