//! Credentials for the two deployments. Neither variant's secret ever appears in `Debug`, an
//! error or a log: [`JiraAuth`]'s `Debug` impl always redacts to `JiraAuth::Basic { email: ***,
//! .. }` / `JiraAuth::Bearer { .. }`, and [`JiraAuth::header_value`] is the only place the real
//! value is exposed — callers must not log or print its result.

use base64::Engine as _;
use std::fmt;

/// How this sync authenticates to one Jira deployment.
#[derive(Clone)]
pub enum JiraAuth {
    /// Jira Cloud: HTTP Basic with the account e-mail and an API token (never the account
    /// password).
    Basic {
        /// The account e-mail. Not itself a secret, but kept out of `Debug` anyway: it is
        /// personally identifying, and pairing it with the token it is redacted next to is an
        /// easy mistake to make later.
        email: String,
        /// The API token.
        api_token: String,
    },
    /// Jira Data Center: a Bearer personal access token.
    Bearer {
        /// The token.
        token: String,
    },
}

impl JiraAuth {
    /// The `Authorization` header value to send. This is the only place either secret is
    /// exposed; callers must not log or print the result.
    #[must_use]
    pub fn header_value(&self) -> String {
        match self {
            JiraAuth::Basic { email, api_token } => {
                let raw = format!("{email}:{api_token}");
                let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
                format!("Basic {encoded}")
            }
            JiraAuth::Bearer { token } => format!("Bearer {token}"),
        }
    }
}

impl fmt::Debug for JiraAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JiraAuth::Basic { .. } => f.write_str("JiraAuth::Basic { email: ***, api_token: *** }"),
            JiraAuth::Bearer { .. } => f.write_str("JiraAuth::Bearer { token: *** }"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_debug_never_shows_the_email_or_token() {
        let auth = JiraAuth::Basic {
            email: "demo@jira.example.com".to_string(),
            api_token: "super-secret-token".to_string(),
        };
        let shown = format!("{auth:?}");
        assert!(!shown.contains("demo@jira.example.com"));
        assert!(!shown.contains("super-secret-token"));
        assert!(shown.contains("***"));
    }

    #[test]
    fn bearer_debug_never_shows_the_token() {
        let auth = JiraAuth::Bearer {
            token: "pat-super-secret".to_string(),
        };
        let shown = format!("{auth:?}");
        assert!(!shown.contains("pat-super-secret"));
        assert!(shown.contains("***"));
    }

    #[test]
    fn basic_header_value_is_base64_of_email_colon_token() {
        let auth = JiraAuth::Basic {
            email: "demo@jira.example.com".to_string(),
            api_token: "tok".to_string(),
        };
        assert_eq!(
            auth.header_value(),
            "Basic ZGVtb0BqaXJhLmV4YW1wbGUuY29tOnRvaw=="
        );
    }

    #[test]
    fn bearer_header_value_is_a_bearer_token() {
        let auth = JiraAuth::Bearer {
            token: "pat-123".to_string(),
        };
        assert_eq!(auth.header_value(), "Bearer pat-123");
    }
}
