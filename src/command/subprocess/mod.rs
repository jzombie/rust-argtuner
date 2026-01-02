pub mod runner;
pub mod talkback;

// Re-export commonly used command subprocess types at `crate::command::subprocess`.
pub use runner::{CommandOutput, CommandResultPayload, CommandRunner};
pub use talkback::{ParsedItem, parse_output, parse_prefix_lines};
