//! Core engine for browser-terminal: shell language, structured values,
//! pipeline evaluation, line editor, renderer, and multiplexer state.
//!
//! This crate has zero wasm dependencies and is tested natively.

/// Milestone-1 placeholder for the input hot path: transform raw input into
/// the bytes to echo back to the terminal (CR becomes CRLF so Enter starts a
/// new line). Replaced by the real `LineEditor` in milestone 3.
pub fn echo_transform(input: &str) -> String {
    input.replace('\r', "\r\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn cr_becomes_crlf() {
        assert_eq!(super::echo_transform("hi\r"), "hi\r\n");
    }
}
