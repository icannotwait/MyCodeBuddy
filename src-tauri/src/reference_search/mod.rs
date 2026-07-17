//! Source-neutral incremental reference search protocol and matcher.
//!
//! Types and ranking live here so file / conversation / commit sources (and
//! candidate validation) share one field mapping and one URI codec. Command
//! handlers in `commands::reference_search` call into this module.

pub mod matcher;
pub mod types;

pub use matcher::{
    build_commit_uri, build_file_uri, build_session_uri, encode_uri_component, match_fields,
    match_reference_candidate, match_reference_regex, SearchPattern,
};
pub use types::*;
