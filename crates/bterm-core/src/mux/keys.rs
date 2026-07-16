//! The prefix keymap: everything is a command. TS only knows the prefix
//! chord; after `Ctrl-B` it forwards the next key here, which resolves to a
//! shell command string evaluated in the active pane.

/// Resolve a post-prefix key (browser `KeyboardEvent.key` values) to a
/// command line. `None` = unbound (disarm silently).
pub fn keymap(key: &str) -> Option<&'static str> {
    Some(match key {
        "%" => "mux split --right",
        "\"" => "mux split --down",
        "c" => "mux window new",
        "n" => "mux window next",
        "p" => "mux window prev",
        "x" => "mux kill-pane",
        "o" => "mux focus next",
        "ArrowLeft" => "mux focus left",
        "ArrowRight" => "mux focus right",
        "ArrowUp" => "mux focus up",
        "ArrowDown" => "mux focus down",
        "z" => "mux zoom",
        "s" => "session list",
        "(" => "session prev",
        ")" => "session next",
        "d" => "mux hide",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_bindings_resolve() {
        assert_eq!(keymap("%"), Some("mux split --right"));
        assert_eq!(keymap("\""), Some("mux split --down"));
        assert_eq!(keymap("ArrowLeft"), Some("mux focus left"));
        assert_eq!(keymap("q"), None);
    }
}
