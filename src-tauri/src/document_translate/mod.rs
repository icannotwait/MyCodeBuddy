//! On-demand document translation (Markdown / plain text).
//!
//! Task 4 owns fail-closed Markdown code protection; later tasks add the
//! runner, service, and commands.

pub mod protect;

pub use protect::{
    protect_markdown, protect_markdown_with_nonce, restore_markdown, ProtectError,
    ProtectedDocument,
};
