//! Zulip server HTML → Puzil's render model.
//!
//! - [`ast`]: the block/inline tree the UI lays out.
//! - [`text_index`]: the Overlay-compatible text of a message.

pub mod ast;
pub mod text_index;

pub use ast::MessageAst;
