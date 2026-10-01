//! Removing anything token-shaped from text that came from outside the app (the daemon's stderr,
//! its ready line, its error messages) before it is logged or shown.
//!
//! - `pcd_…` and `pca_…`: the daemon's device and agent tokens;
//! - `pitcrew.bearer.…`: a token as a WebSocket subprotocol;
//! - `Bearer …`: a token in an `Authorization` header (any case);
//! - any other run of 40 or more base64url characters (a raw secret, a hash).
//!
//! Each becomes its prefix and `…`.

/// Characters a token is made of (base64url, and the `.` and `~` of RFC 6750's b64token).
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '+' | '/' | '=')
}

/// Characters of a bare base64url run.
fn is_run_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_')
}

/// A run of this many base64url characters is treated as a secret.
const LONG_RUN: usize = 40;

/// `text` with every token-shaped part replaced.
#[must_use]
pub fn redact(text: &str) -> String {
    let markers = after_markers(text);
    redact_long_runs(&markers)
}

/// Replaces what follows `pcd_`, `pca_`, `pitcrew.bearer.` and `bearer ` (any case).
fn after_markers(text: &str) -> String {
    const MARKERS: [&str; 4] = ["pcd_", "pca_", "pitcrew.bearer.", "bearer "];
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let hit = MARKERS
            .iter()
            .find(|m| lower[i..].starts_with(*m) && text.is_char_boundary(i + m.len()));
        if let Some(marker) = hit {
            let start = i + marker.len();
            let rest = &text[start..];
            let end = rest
                .char_indices()
                .find(|&(_, c)| !is_token_char(c))
                .map_or(rest.len(), |(j, _)| j);
            out.push_str(&text[i..start]);
            if end > 0 {
                out.push('…');
            }
            i = start + end;
        } else {
            let c = text[i..].chars().next().unwrap_or(' ');
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// Replaces runs of [`LONG_RUN`] or more base64url characters.
fn redact_long_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.chars().count() >= LONG_RUN {
            out.push('…');
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if is_run_char(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "pcd_ZmFrZS10b2tlbi1mb3ItdGhlLXJlZGFjdGlvbi10ZXN0LTEyMzQ1";

    #[test]
    fn tokens_are_removed_wherever_they_appear() {
        let body = &TOKEN[4..];
        for (input, expected) in [
            (
                format!("the token is {TOKEN}"),
                "the token is pcd_…".to_owned(),
            ),
            (format!("{TOKEN},x"), "pcd_…,x".to_owned()),
            (
                "agent pca_abc-DEF_123 done".to_owned(),
                "agent pca_… done".to_owned(),
            ),
            (
                format!("Sec-WebSocket-Protocol: pitcrew.v1, pitcrew.bearer.{TOKEN}"),
                "Sec-WebSocket-Protocol: pitcrew.v1, pitcrew.bearer.…".to_owned(),
            ),
            (
                format!("authorization: Bearer {TOKEN}"),
                "authorization: Bearer …".to_owned(),
            ),
            (format!("BEARER {body}"), "BEARER …".to_owned()),
            (
                format!("raw secret {body} here"),
                "raw secret … here".to_owned(),
            ),
        ] {
            let out = redact(&input);
            assert_eq!(out, expected, "{input}");
            assert!(!out.contains(body), "{out}");
        }
    }

    #[test]
    fn ordinary_lines_are_kept() {
        for line in [
            "pitcrewd: another pitcrewd is already running on /home/sam/.local/share/pitcrew",
            "2026-09-30T11:00:00Z INFO pitcrew_api: listening workspace=01JA0000000000000000000000",
            "exit status: 3",
            "",
            "naïve ünïcode — fine",
        ] {
            assert_eq!(redact(line), line);
        }
    }
}
