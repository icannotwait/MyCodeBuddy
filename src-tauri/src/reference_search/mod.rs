//! Source-neutral incremental reference search protocol and matcher.
//!
//! Types and ranking live here so file / conversation / commit sources (and
//! candidate validation) share one field mapping and one URI codec. Command
//! handlers in `commands::reference_search` call into this module. The
//! registry owns guarded pull jobs; production cursors and validation live
//! under `sources` / `validation`.

pub mod matcher;
pub mod registry;
pub mod sources;
pub mod types;
pub mod validation;

pub use matcher::{
    build_commit_uri, build_file_uri, build_session_uri, encode_uri_component, match_fields,
    match_reference_candidate, match_reference_regex, SearchPattern,
};
pub use registry::{
    run_reference_search_sweeper, ProductionReferenceSourceFactory, ReferenceSearchRegistry,
};
pub use sources::{ReferenceSourceCursor, ReferenceSourceFactory, SourcePage};
pub use types::*;
pub use validation::validate_reference_candidate_core;
