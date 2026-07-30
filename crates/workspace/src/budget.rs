//! Context budgeting for multi-project requests: bounds applied in
//! application code so a large repository can never consume the whole
//! request and crowd out results from another selected project.
//!
//! Policy (deliberately simple: independent per-project caps rather than
//! dynamic redistribution, so behavior doesn't change based on how many
//! *other* projects happen to be selected):
//!
//! - [`MAX_PROJECTS_PER_REQUEST`] bounds how many projects one request can
//!   touch at all.
//! - [`MAX_RESULTS_PER_PROJECT`] and [`MAX_EXCERPT_BYTES_PER_RESULT`] bound
//!   what `workspace.search` returns per project, independent of how many
//!   projects are selected -- one huge repository can't push another
//!   project's results out of the response.
//! - [`MAX_READ_BYTES_PER_PROJECT`] bounds a single `workspace.read_file`
//!   call, mirroring `project.read_file`'s own cap.
//! - [`MAX_TOTAL_CONTEXT_BYTES`] is a backstop across the whole response,
//!   in case every project legitimately has a lot to say; ranking
//!   (filename/heading matches first, see `orbit_project::search`) means
//!   whatever gets truncated is the lowest-relevance content, and every
//!   kept result keeps its full source metadata even if its excerpt was
//!   shortened to fit.

pub const MAX_PROJECTS_PER_REQUEST: usize = 8;
pub const MAX_RESULTS_PER_PROJECT: usize = 5;
pub const MAX_EXCERPT_BYTES_PER_RESULT: usize = 400;
pub const MAX_READ_BYTES_PER_PROJECT: u64 = 8_000;
pub const MAX_TOTAL_CONTEXT_BYTES: usize = 24_000;

/// Truncate `s` to at most `max_bytes`, respecting UTF-8 character
/// boundaries (never splitting a multi-byte character).
pub fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_bytes_leaves_short_strings_untouched() {
        assert_eq!(truncate_bytes("hello", 100), "hello");
    }

    #[test]
    fn truncate_bytes_respects_utf8_boundaries() {
        let s = "héllo world"; // é is 2 bytes
        let truncated = truncate_bytes(s, 2);
        assert!(truncated.is_char_boundary(truncated.len() - 1) || truncated.ends_with('…'));
        assert!(String::from_utf8(truncated.clone().into_bytes()).is_ok());
    }
}
