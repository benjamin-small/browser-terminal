//! Events crossing the engine → host boundary. The TS wrapper receives
//! these as a tagged union through one callback. (HostMsg — host → engine —
//! joins in the multiplexer milestone.)

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EngineEvent {
    /// Bytes to write to a pane's terminal (already `\r\n`-converted).
    PaneOutput { pane: u32, data: String },
    /// The engine hit a non-recoverable state; the wrapper should disable input.
    Fatal { message: String },
}
