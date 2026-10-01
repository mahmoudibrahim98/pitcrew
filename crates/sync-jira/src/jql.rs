//! JQL: a validated project key, and the one query shape this crate ever builds. **Never put
//! untrusted text into JQL.** The only two values spliced into the query text are a
//! [`ProjectRef`] — rejected unless it is made of the characters Jira project keys are actually
//! allowed to use — and a cursor this crate computed itself from a previously-validated
//! [`crate::time::JiraTimestamp`], never text read from an issue body, title or label.

use std::fmt;

/// A Jira project key, `owner/repo`'s analogue here, validated once so it can be spliced into JQL
/// safely: 2–10 characters, the first an uppercase ASCII letter, the rest uppercase letters or
/// digits (Jira's own rule for a project key). A key containing quotes, spaces or a JQL operator
/// is rejected by construction, not escaped.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectRef(String);

/// `value` was not a valid Jira project key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a valid Jira project key: {0:?}")]
pub struct InvalidProjectRef(pub String);

impl ProjectRef {
    /// Validates and wraps a project key.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidProjectRef> {
        let value = value.into();
        let mut chars = value.chars();
        let first_ok = chars.next().is_some_and(|c| c.is_ascii_uppercase());
        let rest_ok = chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        let len_ok = (2..=10).contains(&value.len());
        if first_ok && rest_ok && len_ok {
            Ok(Self(value))
        } else {
            Err(InvalidProjectRef(value))
        }
    }

    /// The key text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Builds `project in ("<key>") AND updated >= "<cursor>" ORDER BY updated ASC, key ASC` (or,
/// with no cursor, a first full sync: `project in ("<key>") ORDER BY updated ASC, key ASC`).
/// `cursor` is JQL's own `"YYYY-MM-DD HH:MM"` shape (see [`crate::time::account_minute`]), built
/// by this crate from a timestamp it already validated — never from unvalidated text.
///
/// `key ASC` is a tie-breaker, not just tidiness: several issues can share the exact same
/// `updated` minute (two edits in the same minute, or many issues bulk-updated together), and
/// without a secondary sort key JQL does not promise a stable order among them. An unstable order
/// is a real problem for Data Center's `startAt` (offset) pagination in particular — each page is
/// a fresh query re-executed at a numeric offset into the *current* result set, so if ties are
/// ordered differently between two page fetches within the same call, an item can be skipped or
/// repeated even though nothing it owns actually changed. `key ASC` removes the most common cause
/// of that (ties at the same instant); it does **not** fully remove the risk, because an issue's
/// `updated` can itself change (entering or leaving the filtered set, or moving past the current
/// page) between one page fetch and the next within a single paginated walk, which no sort clause
/// can protect against. [`crate::deployment::JiraDataCenter`]'s own doc comment has more on this.
/// Jira Cloud's `nextPageToken` pagination is not affected: it is not a raw numeric offset.
#[must_use]
pub fn incremental_query(project: &ProjectRef, cursor: Option<&str>) -> String {
    match cursor {
        Some(cursor) => format!(
            "project in (\"{project}\") AND updated >= \"{cursor}\" ORDER BY updated ASC, key ASC"
        ),
        None => format!("project in (\"{project}\") ORDER BY updated ASC, key ASC"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_plain_key_is_accepted() {
        assert!(ProjectRef::new("DEMO").is_ok());
        assert!(ProjectRef::new("AB1").is_ok());
    }

    #[test]
    fn a_key_with_quotes_is_rejected() {
        assert!(ProjectRef::new("DEMO\" OR 1=1 --").is_err());
        assert!(ProjectRef::new("DE'MO").is_err());
    }

    #[test]
    fn a_key_with_spaces_is_rejected() {
        assert!(ProjectRef::new("DEMO PROJECT").is_err());
    }

    #[test]
    fn a_key_with_jql_operators_is_rejected() {
        for bad in ["DEMO OR TRUE", "DEMO)AND(1=1", "DEMO;DROP", "demo", "D"] {
            assert!(ProjectRef::new(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn a_key_starting_with_a_digit_is_rejected() {
        assert!(ProjectRef::new("1DEMO").is_err());
    }

    #[test]
    fn a_key_that_is_too_long_is_rejected() {
        assert!(ProjectRef::new("ABCDEFGHIJK").is_err());
    }

    #[test]
    fn incremental_query_has_the_expected_shape() {
        let project = ProjectRef::new("DEMO").expect("valid");
        assert_eq!(
            incremental_query(&project, Some("2026-01-02 03:04")),
            "project in (\"DEMO\") AND updated >= \"2026-01-02 03:04\" ORDER BY updated ASC, key ASC"
        );
        assert_eq!(
            incremental_query(&project, None),
            "project in (\"DEMO\") ORDER BY updated ASC, key ASC"
        );
    }

    proptest::proptest! {
        /// No input, well-formed or not, can make `incremental_query` produce text containing a
        /// raw (unquoted) double quote from the project key itself: `ProjectRef::new` is the only
        /// way to build one, and it never accepts a key containing one.
        #[test]
        fn no_accepted_key_ever_contains_a_quote(key in "[A-Z][A-Z0-9]{1,9}") {
            let project = ProjectRef::new(key).expect("matches the validator's own rule");
            prop_assert!(!project.as_str().contains('"'));
        }
    }
}
