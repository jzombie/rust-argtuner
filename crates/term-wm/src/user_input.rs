use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

#[derive(Default)]
pub struct InputNormalizer {
    esc_down: bool,
}

pub fn normalize_event(evt: Event, normalizer: &mut InputNormalizer) -> Option<Event> {
    match evt {
        Event::Key(mut key) => {
            if key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT) {
                key.code = KeyCode::BackTab;
                key.modifiers.remove(KeyModifiers::SHIFT);
            }
            if cfg!(windows) {
                match key.kind {
                    KeyEventKind::Release => {
                        if key.code == KeyCode::Esc {
                            normalizer.esc_down = false;
                        }
                        return None;
                    }
                    KeyEventKind::Repeat => return None,
                    KeyEventKind::Press => {}
                }
                if key.code == KeyCode::Esc {
                    if normalizer.esc_down {
                        return None;
                    }
                    normalizer.esc_down = true;
                } else {
                    normalizer.esc_down = false;
                }
            } else if key.kind == KeyEventKind::Release {
                return None;
            }
            Some(Event::Key(key))
        }
        other => Some(other),
    }
}
