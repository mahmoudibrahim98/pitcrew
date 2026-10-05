//! Redaction: what never leaves PitCrew in a prompt.
//!
//! Text from sessions (titles, recap lines, file paths, branch names) and from people (task and
//! workstream names) can hold secrets a CLI or a person typed: a token in a command line, a
//! password in a URL, a key pasted into a title. Before any of it goes to an agent, [`line`] makes
//! it one clean line and replaces what looks like a secret, an e-mail address or a person's home
//! folder. The rules are deliberately broad: a false positive costs a word of context, a false
//! negative a secret.
//!
//! Replaced, as `[redacted]` (each counts once):
//! - a private key block (`-----BEGIN … PRIVATE KEY-----`): the whole text;
//! - well-known token shapes, by prefix: `sk-`, `sk_live_`, `ghp_`, `github_pat_`, `glpat-`,
//!   `xoxb-`, `AKIA…`, `AIza…`, PitCrew's own `pcd_`/`pca_`/`pcr_`, and others ([`PREFIXES`]);
//! - JSON Web Tokens (`eyJ….eyJ….…`);
//! - the value after a secret's name: `password=…`, `token: …`, `api_key=…`, `?access_token=…`,
//!   `--password …`, and the word after `Bearer` or `Basic`;
//! - the user and password of a URL (`https://user:pass@host` → `https://[redacted]@host`);
//! - long random-looking words: 32 or more characters of letters, digits and `+/=_-` with
//!   upper- and lowercase letters and digits, or 32 or more hexadecimal digits with letters and
//!   digits;
//! - e-mail addresses, as `[email]`;
//! - a home folder: `/home/<name>`, `/Users/<name>` and `C:\Users\<name>` become `~`, and
//!   `/root` too.
//!
//! Control and hidden characters (`pitcrew_protocol::text::is_hidden`) are dropped, and
//! whitespace runs become one space, before anything is matched, so a secret cannot hide behind
//! them.

use pitcrew_protocol::text::{is_hidden, is_line_separator};

/// What a secret is replaced with.
pub const REDACTED: &str = "[redacted]";
/// What an e-mail address is replaced with.
pub const EMAIL: &str = "[email]";

/// Token prefixes that mark a credential, matched case-sensitively, each followed by at least
/// [`MIN_AFTER_PREFIX`] token characters.
pub const PREFIXES: &[&str] = &[
    "sk-",
    "sk_live_",
    "sk_test_",
    "rk_live_",
    "rk_test_",
    "pk_live_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "gldt-",
    "xoxa-",
    "xoxb-",
    "xoxp-",
    "xoxr-",
    "xoxs-",
    "xapp-",
    "AKIA",
    "ASIA",
    "AIza",
    "ya29.",
    "pcd_",
    "pca_",
    "pcr_",
    "npm_",
    "pypi-",
    "hf_",
    "dop_v1_",
    "doo_v1_",
    "shpat_",
    "shpss_",
    "SG.",
    "glc_",
    "sq0atp-",
    "EAAC",
    "ATATT",
];

/// Token characters after a [`PREFIXES`] prefix for it to count as one.
pub const MIN_AFTER_PREFIX: usize = 8;

/// The shortest random-looking word replaced.
pub const MIN_RANDOM: usize = 32;

/// Names whose value is a secret, matched within a lowercased name (`DB_PASSWORD`, `x-api-key`).
const SECRET_NAMES: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "apikey",
    "api_key",
    "api-key",
    "access_key",
    "access-key",
    "private_key",
    "private-key",
    "credential",
    "authorization",
    "session_key",
    "cookie",
];

/// Exact names (lowercased) whose value is a secret, too short to match within others.
const SECRET_EXACT: &[&str] = &["pwd", "pw", "pat", "key", "sig", "signature"];

/// Words that name how the next word authenticates (`Authorization: Bearer <token>`).
const SCHEMES: &[&str] = &["bearer", "basic", "digest"];

/// The longest a text is let grow before matching, as a multiple of what is kept.
const SCAN_FACTOR: usize = 4;

/// What [`line`] made of a text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Redacted {
    /// The text: one line, at most the characters asked for.
    pub text: String,
    /// How many things were replaced.
    pub count: u32,
}

/// `text` as one clean line of at most `max` characters, with every secret, e-mail address and
/// home folder replaced (see the [module docs](self)). A text that was cut ends with `…`.
///
/// Work is bounded by `max`: at most `4 × max + 64` characters are kept for matching, so a secret
/// cut there starts past `max` or is still long enough to match.
#[must_use]
pub fn line(text: &str, max: usize) -> Redacted {
    if max == 0 {
        return Redacted::default();
    }
    let limit = max.saturating_mul(SCAN_FACTOR).saturating_add(64);
    let (tidy, cut) = tidy(text, limit);
    let mut out = redact(&tidy);
    let chars = out.text.chars().count();
    if chars > max || (cut && chars == max) {
        let mut kept: String = out.text.chars().take(max.saturating_sub(1)).collect();
        kept.truncate(kept.trim_end().len());
        kept.push('…');
        out.text = kept;
    } else if cut {
        out.text.push('…');
    }
    out
}

/// Hidden characters dropped, control characters and whitespace runs as one space, the ends
/// trimmed; at most `limit` characters. Whether it was cut.
fn tidy(text: &str, limit: usize) -> (String, bool) {
    let mut out = String::new();
    let mut count = 0usize;
    let mut space = false;
    for c in text.chars() {
        if is_hidden(c) && !is_line_separator(c) {
            continue;
        }
        if c.is_whitespace() || c.is_control() || is_line_separator(c) {
            space = count > 0;
            continue;
        }
        if count + usize::from(space) + 1 > limit {
            return (out, true);
        }
        if space {
            out.push(' ');
            count += 1;
            space = false;
        }
        out.push(c);
        count += 1;
    }
    (out, false)
}

/// Every rule over one tidy line.
fn redact(text: &str) -> Redacted {
    if text.contains("PRIVATE KEY") && text.contains("-----BEGIN") {
        return Redacted {
            text: REDACTED.to_owned(),
            count: 1,
        };
    }
    let mut count = 0u32;
    let mut out = String::with_capacity(text.len());
    // Whether the word before asked for its value: `Bearer`, `password:`, `--token`.
    let mut value_next = false;
    for piece in pieces(text) {
        match piece {
            Piece::Gap(gap) => out.push_str(gap),
            Piece::Word(word) => {
                let (core, tail) = split_tail(word);
                let scheme = SCHEMES.contains(&core.to_ascii_lowercase().as_str());
                let replaced = if value_next && !scheme && is_value(core) {
                    count += 1;
                    REDACTED.to_owned()
                } else {
                    word_rules(core, &mut count)
                };
                value_next = asks_for_value(core, tail);
                out.push_str(&replaced);
                out.push_str(tail);
            }
        }
    }
    let (text, homes) = homes(&out);
    count += homes;
    Redacted { text, count }
}

enum Piece<'a> {
    Word(&'a str),
    Gap(&'a str),
}

/// Characters that end a word: whitespace, quotes, brackets, commas and semicolons.
fn is_gap(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | '|'
        )
}

/// The text as words and the gaps between them, in order.
fn pieces(text: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_gap: Option<bool> = None;
    for (i, c) in text.char_indices() {
        let gap = is_gap(c);
        match in_gap {
            Some(was) if was == gap => {}
            Some(was) => {
                out.push(if was {
                    Piece::Gap(&text[start..i])
                } else {
                    Piece::Word(&text[start..i])
                });
                start = i;
            }
            None => {}
        }
        in_gap = Some(gap);
    }
    if let Some(was) = in_gap {
        out.push(if was {
            Piece::Gap(&text[start..])
        } else {
            Piece::Word(&text[start..])
        });
    }
    out
}

/// A word without the sentence punctuation after it, and that punctuation.
fn split_tail(word: &str) -> (&str, &str) {
    let core = word.trim_end_matches(['.', ':', '!', '?']);
    (core, &word[core.len()..])
}

/// Whether a word after a secret's name is its value.
fn is_value(core: &str) -> bool {
    core.chars().count() >= 3 && !core.starts_with('-')
}

/// Whether the word after `core` (followed by `tail`) is a secret's value: after `Bearer` or
/// `Basic`, after a secret's name ending in `:` or `=`, and after a `--password`-like option.
fn asks_for_value(core: &str, tail: &str) -> bool {
    let lower = core.to_ascii_lowercase();
    if SCHEMES.contains(&lower.as_str()) {
        return true;
    }
    let named = lower.trim_end_matches(['=', ':']);
    let option = named.trim_start_matches('-');
    let ends_named = tail.starts_with(':') || core.ends_with('=') || core.ends_with(':');
    (ends_named || named.len() != option.len()) && !option.is_empty() && is_secret_name(option)
}

fn is_secret_name(name: &str) -> bool {
    let name = name.trim_start_matches('$').to_ascii_lowercase();
    SECRET_EXACT.contains(&name.as_str()) || SECRET_NAMES.iter().any(|s| name.contains(s))
}

/// The rules for one word, on its own.
fn word_rules(core: &str, count: &mut u32) -> String {
    if core.is_empty() {
        return String::new();
    }
    if is_email(core) {
        *count += 1;
        return EMAIL.to_owned();
    }
    if let Some(url) = url_userinfo(core) {
        *count += 1;
        return pairs(&url, count);
    }
    if core.contains('=') {
        return pairs(core, count);
    }
    if let Some((name, value)) = core.split_once(':')
        && !value.is_empty()
        && !value.starts_with("//")
        && is_secret_name(name)
    {
        *count += 1;
        return format!("{name}:{REDACTED}");
    }
    scrub(core, count)
}

/// `name=value` pairs in a word (a query string, an assignment, a cookie), with the values of
/// secrets' names replaced; and the word's other parts [scrubbed](scrub).
fn pairs(word: &str, count: &mut u32) -> String {
    let mut out = String::with_capacity(word.len());
    let mut rest = word;
    loop {
        let end = rest.find(['&', '?', '#']).unwrap_or(rest.len());
        let (part, after) = rest.split_at(end);
        match part.split_once('=') {
            Some((name, value)) if !value.is_empty() && is_secret_name(name) => {
                *count += 1;
                out.push_str(name);
                out.push('=');
                out.push_str(REDACTED);
            }
            Some((name, value)) => {
                out.push_str(&scrub(name, count));
                out.push('=');
                out.push_str(&scrub(value, count));
            }
            None => out.push_str(&scrub(part, count)),
        }
        let Some(sep) = after.chars().next() else {
            break;
        };
        out.push(sep);
        rest = &after[sep.len_utf8()..];
    }
    out
}

/// A part of a word: replaced whole when it is an e-mail address or a token; else each of its
/// path segments (between `/` and `\\`) is, when it is one: a secret in a folder or branch name.
fn scrub(part: &str, count: &mut u32) -> String {
    if let Some(replaced) = replacement(part) {
        *count += 1;
        return replaced.to_owned();
    }
    if !part.contains(['/', '\\']) {
        return part.to_owned();
    }
    let mut out = String::with_capacity(part.len());
    let mut start = 0;
    for (i, c) in part.char_indices() {
        if c == '/' || c == '\\' {
            push_segment(&mut out, &part[start..i], count);
            out.push(c);
            start = i + c.len_utf8();
        }
    }
    push_segment(&mut out, &part[start..], count);
    out
}

fn push_segment(out: &mut String, segment: &str, count: &mut u32) {
    match replacement(segment) {
        Some(replaced) => {
            *count += 1;
            out.push_str(replaced);
        }
        None => out.push_str(segment),
    }
}

/// What replaces `part` whole, if it is an e-mail address or a token.
fn replacement(part: &str) -> Option<&'static str> {
    if part.is_empty() {
        None
    } else if is_email(part) {
        Some(EMAIL)
    } else if is_token(part) {
        Some(REDACTED)
    } else {
        None
    }
}

/// `scheme://user:pass@host…` with its user and password replaced, if it has them.
fn url_userinfo(word: &str) -> Option<String> {
    let (scheme, rest) = word.split_once("://")?;
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let at = rest[..host_end].rfind('@')?;
    Some(format!("{scheme}://{REDACTED}{}", &rest[at..]))
}

/// `local@domain.tld`, with a non-empty local part.
fn is_email(word: &str) -> bool {
    let Some((local, domain)) = word.rsplit_once('@') else {
        return false;
    };
    let local = local.trim_start_matches(['<', '(', ':']);
    !local.is_empty()
        && !local.contains('/')
        && local
            .chars()
            .all(|c| c.is_alphanumeric() || "._%+-!#$&*=^{}~".contains(c))
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-')
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '=' | '.')
}

/// Whether a word (or part of one) looks like a credential.
fn is_token(word: &str) -> bool {
    for prefix in PREFIXES {
        let Some(rest) = word.strip_prefix(prefix) else {
            continue;
        };
        let run: String = rest.chars().take_while(|c| is_token_char(*c)).collect();
        // Real credentials carry digits or mixed case; `sk-learn` and `ASIAN` do not.
        let digits = run.chars().any(|c| c.is_ascii_digit());
        let mixed = run.chars().any(|c| c.is_ascii_uppercase())
            && run.chars().any(|c| c.is_ascii_lowercase());
        if run.chars().count() >= MIN_AFTER_PREFIX && (digits || mixed) {
            return true;
        }
    }
    is_jwt(word) || is_random(word)
}

/// `eyJ…` header, payload and signature, base64url, separated by dots.
fn is_jwt(word: &str) -> bool {
    let parts: Vec<&str> = word.split('.').collect();
    parts.len() == 3
        && parts[0].starts_with("eyJ")
        && word.len() >= 30
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '=')
        })
}

/// A long, random-looking word: see the [module docs](self).
fn is_random(word: &str) -> bool {
    for part in word.split(['.', ':', '@']) {
        if part.len() < MIN_RANDOM {
            continue;
        }
        let digits = part.chars().filter(char::is_ascii_digit).count();
        let hex = part.chars().all(|c| c.is_ascii_hexdigit());
        let letters = part.chars().filter(char::is_ascii_alphabetic).count();
        if hex && digits > 0 && letters > 0 {
            return true;
        }
        let base64 = part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'));
        let upper = part.chars().any(|c| c.is_ascii_uppercase());
        let lower = part.chars().any(|c| c.is_ascii_lowercase());
        // A long path or a long snake-case name has separators every few letters; a key does not.
        let longest_run = part.split(['/', '_', '-']).map(str::len).max().unwrap_or(0);
        if base64 && upper && lower && digits > 0 && longest_run >= 20 {
            return true;
        }
    }
    false
}

/// A person's home folder replaced by `~`: `/home/<name>`, `/Users/<name>`, `C:\Users\<name>`
/// (any drive), and `/root`. How many were.
fn homes(text: &str) -> (String, u32) {
    let mut out = String::with_capacity(text.len());
    let mut count = 0u32;
    let mut rest = text;
    while !rest.is_empty() {
        match home_at(rest) {
            Some(len) if at_boundary(&out) => {
                count += 1;
                out.push('~');
                rest = &rest[len..];
            }
            _ => {
                let Some(c) = rest.chars().next() else {
                    break;
                };
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    (out, count)
}

/// Whether what came before can start a path: nothing, a gap, `=`, `:` or `~`'s neighbours.
fn at_boundary(before: &str) -> bool {
    before
        .chars()
        .next_back()
        .is_none_or(|c| is_gap(c) || matches!(c, '=' | ':' | '@'))
}

/// The length of the home folder `text` starts with, if it does.
fn home_at(text: &str) -> Option<usize> {
    for prefix in ["/home/", "/Users/", "/users/"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let name = rest
                .find(|c: char| c == '/' || is_gap(c))
                .unwrap_or(rest.len());
            if name > 0 {
                return Some(prefix.len() + name);
            }
        }
    }
    if let Some(rest) = text.strip_prefix("/root")
        && rest.chars().next().is_none_or(|c| c == '/' || is_gap(c))
    {
        return Some("/root".len());
    }
    let bytes = text.as_bytes();
    if bytes.len() > 9
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
        && text[3..8].eq_ignore_ascii_case("users")
        && matches!(bytes[8], b'\\' | b'/')
    {
        let rest = &text[9..];
        let name = rest
            .find(|c: char| c == '\\' || c == '/' || is_gap(c))
            .unwrap_or(rest.len());
        if name > 0 {
            return Some(9 + name);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(text: &str) -> String {
        line(text, 500).text
    }

    #[test]
    fn plain_text_passes_unchanged() {
        for text in [
            "Draft the method section",
            "Ran cargo test -p pitcrew-store: 212 of 240 passed",
            "Edited src/watch.rs (+12 −3)",
            "PAP-4 moved to review",
            "Seed runs 1–5 on the cluster",
            "Fix the parser for 2026-10-01 timestamps at 10:30",
            "commit 4815162342 and job 4815164",
            "ask @sam about the figure",
            "session 01JB000000000000000SES0001",
            "uuid 3f2b1c4e-8d7a-4e6f-9b0c-1d2e3f4a5b6c in the log",
            "src/very_long_module_name/with_a_longer_file_name_v2.rs",
        ] {
            let out = line(text, 500);
            assert_eq!(
                out,
                Redacted {
                    text: text.to_owned(),
                    count: 0
                },
                "{text}"
            );
        }
    }

    /// Checks that `text` redacts to `want`. A failure names the case only, never the texts or
    /// what came out: they hold synthetic secrets, and a failure must not print those either.
    #[track_caller]
    fn redacts(case: &str, text: &str, want: &str) {
        assert!(r(text) == want, "{case}: not redacted as expected");
    }

    #[test]
    fn known_tokens_are_replaced() {
        for (case, text, want) in [
            (
                "an API key in an assignment",
                "export OPENAI_API_KEY=sk-proj-abcdefghijklmnop1234",
                "export OPENAI_API_KEY=[redacted]",
            ),
            (
                "an sk- key",
                "key sk-ant-api03-AbCdEfGhIjKlMnOp",
                "key [redacted]",
            ),
            (
                "a GitHub token",
                "push with ghp_16C7e42F292c6912E7710c838347Ae178B4a",
                "push with [redacted]",
            ),
            (
                "a fine-grained GitHub token",
                "token github_pat_11ABCDEFG0123456789_abcdefghijklmnop",
                "token [redacted]",
            ),
            (
                "a Slack token",
                "slack xoxb-123456789012-abcdefghijkl",
                "slack [redacted]",
            ),
            (
                "an AWS key id",
                "aws AKIAIOSFODNN7EXAMPLE done",
                "aws [redacted] done",
            ),
            (
                "a Google key",
                "maps AIzaSyD-1234567890abcdefghijklmnopqrstu",
                "maps [redacted]",
            ),
            (
                "a PitCrew token",
                "pitcrew pca_Zm9vYmFyYmF6cXV4",
                "pitcrew [redacted]",
            ),
            (
                "a GitLab token",
                "gitlab glpat-xxxxyyyyzzzz1234",
                "gitlab [redacted]",
            ),
            (
                "words that only start like tokens",
                "uses sk-learn-tutorial and ASIAN data",
                "uses sk-learn-tutorial and ASIAN data",
            ),
        ] {
            redacts(case, text, want);
        }
    }

    #[test]
    fn named_values_are_replaced() {
        for (case, text, want) in [
            (
                "a bearer header",
                "curl -H 'Authorization: Bearer abc.def.ghi' x",
                "curl -H 'Authorization: Bearer [redacted]' x",
            ),
            (
                "an assignment",
                "password=hunter22 and more",
                "password=[redacted] and more",
            ),
            (
                "a name and a colon",
                "DB_PASSWORD: hunter22",
                "DB_PASSWORD: [redacted]",
            ),
            (
                "an option",
                "login --password hunter22 --user sam",
                "login --password [redacted] --user sam",
            ),
            (
                "a query string",
                "GET /api?user=sam&access_token=abcdef123&page=2",
                "GET /api?user=sam&access_token=[redacted]&page=2",
            ),
            (
                "a name and a colon in one word",
                "secret:topsecretvalue",
                "secret:[redacted]",
            ),
            (
                "basic credentials",
                "Basic dXNlcjpwYXNz",
                "Basic [redacted]",
            ),
            (
                "an environment variable",
                "set PGPASSWORD=s3cr3t-value",
                "set PGPASSWORD=[redacted]",
            ),
            (
                "words that only look like names",
                "author: sam, tokens used 1200",
                "author: sam, tokens used 1200",
            ),
        ] {
            redacts(case, text, want);
        }
    }

    #[test]
    fn urls_lose_their_credentials() {
        redacts(
            "a URL's user and password",
            "clone https://sam:hunter2@git.example.com/lab/repo.git now",
            "clone https://[redacted]@git.example.com/lab/repo.git now",
        );
        redacts(
            "a URL without credentials",
            "see https://example.com/a?b=c",
            "see https://example.com/a?b=c",
        );
    }

    #[test]
    fn jwts_and_random_words_are_replaced() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        redacts(
            "a JWT as a value",
            &format!("token={jwt}"),
            "token=[redacted]",
        );
        redacts(
            "a JWT on its own",
            &format!("sent {jwt}"),
            "sent [redacted]",
        );
        redacts(
            "a random word",
            "key Zx9Qm2Lp8Rt4Vw6Yb1Nc3Hd5Jf7Kg0Ab",
            "key [redacted]",
        );
        redacts(
            "a long hexadecimal word",
            "hex 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "hex [redacted]",
        );
    }

    #[test]
    fn secrets_in_paths_and_branches_are_replaced() {
        redacts(
            "a key as a folder",
            "/home/sam/work/sk-ant-api03-AbCdEfGh12345678/notes.md",
            "~/work/[redacted]/notes.md",
        );
        redacts(
            "an address in a branch",
            "branch fix/sam@example.com",
            "branch fix/[email]",
        );
        redacts(
            "a key id in a branch",
            "fix/AKIAIOSFODNN7EXAMPLE",
            "fix/[redacted]",
        );
        redacts(
            "a token as a Windows folder",
            r"C:\Users\sam\ghp_16C7e42F292c6912E7710c838347Ae178B4a\x",
            r"~\[redacted]\x",
        );
    }

    #[test]
    fn emails_and_homes_are_replaced() {
        assert_eq!(r("mail sam@example.com today"), "mail [email] today");
        assert_eq!(r("ask @sam"), "ask @sam");
        assert_eq!(
            r("cd /home/sam/work/paper && ls /Users/sam/Documents"),
            "cd ~/work/paper && ls ~/Documents"
        );
        assert_eq!(
            r(r"opened C:\Users\sam\work\notes.md"),
            r"opened ~\work\notes.md"
        );
        assert_eq!(r("in /root/.config"), "in ~/.config");
        assert_eq!(r("in /rooted/x and a/home/b"), "in /rooted/x and a/home/b");
        assert_eq!(line("cwd=/home/sam", 100).count, 1);
    }

    #[test]
    fn private_keys_replace_everything() {
        let out = line(
            "-----BEGIN OPENSSH PRIVATE KEY----- b3BlbnNzaC1rZXktdjEAAAAA -----END OPENSSH PRIVATE KEY-----",
            500,
        );
        assert!(
            out.text == REDACTED && out.count == 1,
            "a private key block is not replaced whole"
        );
    }

    #[test]
    fn hidden_and_control_characters_cannot_split_a_secret() {
        redacts(
            "a token split by a zero-width space",
            "ghp_\u{200B}16C7e42F292c6912E7710c838347Ae178B4a",
            "[redacted]",
        );
        assert_eq!(r("pass\u{0}word"), "pass word");
        assert_eq!(r("a\n\tb\u{2028}c"), "a b c");
        redacts(
            "a value after a direction mark",
            "password=\u{202E}hunter22",
            "password=[redacted]",
        );
    }

    #[test]
    fn lines_are_bounded_after_redaction() {
        let long = format!("{} ghp_{}", "word ".repeat(10), "A1b2C3d4".repeat(20));
        let out = line(&long, 40);
        assert!(out.text.chars().count() <= 40, "longer than its bound");
        assert!(!out.text.contains("A1b2C3d4"), "a cut token leaked");
        // A secret that starts before the cut is still long enough to match.
        let out = line(&format!("x ghp_{}", "A1b2C3d4".repeat(200)), 40);
        assert!(
            !out.text.contains("A1b2"),
            "a token cut at the scan's bound leaked"
        );
        assert_eq!(line("short", 40).text, "short");
        assert_eq!(line("a b c d e f", 5).text, "a b…");
        assert_eq!(line(&" ".repeat(100_000), 10).text, "");
        assert_eq!(line("anything", 0), Redacted::default());
    }
}
