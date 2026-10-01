//! Bounds: caps that keep one sync call finite regardless of what the server sends.
//!
//! The generic caps and helpers (title/body/label length caps, the page/item caps, capped
//! backoff) are the same shape sync-github already needed, so they are reused from there rather
//! than redefined here — see the crate README for the reuse decision. Only the caps specific to
//! this crate's own wire shape (ADF depth and node-count) are added locally.

pub use pitcrew_sync_github::bounds::{
    MAX_BODY_CHARS, MAX_LABEL_CHARS, MAX_LABELS, MAX_PAGE_BODY_BYTES, MAX_PAGES_PER_CALL,
    MAX_TITLE_CHARS, SECONDARY_BACKOFF_BASE_SECS, SECONDARY_BACKOFF_CAP_SECS, backoff_secs,
    cap_chars, cap_labels,
};

/// Stop collecting items after this many, across all pages, in one project's sync call. Jira
/// search pages are smaller than GitHub's (see [`MAX_RESULTS_PER_PAGE`]), so this is lower too.
pub const MAX_ITEMS_PER_SYNC: usize = 1_000;

/// `maxResults` requested per search page.
pub const MAX_RESULTS_PER_PAGE: u32 = 100;

/// Stop descending into an Atlassian Document Format tree past this depth. ADF is attacker- or
/// bug-controlled input (a description anyone with issue access can write); without a cap, a
/// deeply nested document could blow the stack or take unbounded time to flatten to text.
pub const MAX_ADF_DEPTH: usize = 32;

/// Stop walking an ADF tree after visiting this many nodes in total, regardless of depth: a
/// document that is wide rather than deep (many thousands of sibling nodes at shallow depth)
/// must not be allowed to take unbounded work either.
pub const MAX_ADF_NODES: usize = 20_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reused_helpers_are_reachable() {
        assert_eq!(cap_chars("hello", 3), "hel");
        assert_eq!(cap_labels(&["a".to_string()]).len(), 1);
        assert!(backoff_secs(1) > 0);
    }
}
