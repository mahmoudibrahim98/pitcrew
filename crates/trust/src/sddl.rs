//! Integrity levels and how SDDL writes them: the mandatory label of a pipe or a token. Text
//! only, so it builds and is tested on every platform; [`crate::windows`] reads and writes the
//! labels themselves.

/// The integrity level of a sandboxed (low) process.
pub const LOW_INTEGRITY: u32 = 0x1000;
/// The integrity level of a normal, non-elevated process.
pub const MEDIUM_INTEGRITY: u32 = 0x2000;
/// The integrity level of an elevated process.
pub const HIGH_INTEGRITY: u32 = 0x3000;
/// The integrity level of system services.
pub const SYSTEM_INTEGRITY: u32 = 0x4000;

/// The integrity level an integrity SID (`S-1-16-<level>`) names.
pub fn integrity_rid(sid: &str) -> Option<u32> {
    sid.strip_prefix("S-1-16-")?.parse().ok()
}

/// The integrity level in an SDDL mandatory label (`S:(ML;;NWNR;;;HI)`), if there is one.
pub fn parse_label(sddl: &str) -> Option<u32> {
    let start = sddl.find("(ML;")?;
    let ace = &sddl[start + 1..];
    let ace = &ace[..ace.find(')')?];
    match ace.rsplit(';').next()? {
        "LW" => Some(LOW_INTEGRITY),
        "ME" => Some(MEDIUM_INTEGRITY),
        "MP" => Some(MEDIUM_INTEGRITY + 0x100),
        "HI" => Some(HIGH_INTEGRITY),
        "SI" => Some(SYSTEM_INTEGRITY),
        sid => integrity_rid(sid),
    }
}

/// How a mandatory label names `integrity` in SDDL: `LW`, `ME`, `HI`, `SI`, or `S-1-16-<level>`.
pub fn label(integrity: u32) -> String {
    match integrity {
        LOW_INTEGRITY => "LW".to_owned(),
        MEDIUM_INTEGRITY => "ME".to_owned(),
        HIGH_INTEGRITY => "HI".to_owned(),
        SYSTEM_INTEGRITY => "SI".to_owned(),
        other => format!("S-1-16-{other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_parse_and_print() {
        assert_eq!(parse_label("S:(ML;;NWNR;;;HI)"), Some(HIGH_INTEGRITY));
        assert_eq!(parse_label("S:(ML;;NW;;;ME)"), Some(MEDIUM_INTEGRITY));
        assert_eq!(parse_label("S:(ML;;NWNR;;;LW)"), Some(LOW_INTEGRITY));
        assert_eq!(parse_label("S:(ML;;NWNR;;;SI)"), Some(SYSTEM_INTEGRITY));
        assert_eq!(parse_label("S:(ML;;NW;;;MP)"), Some(0x2100));
        assert_eq!(parse_label("S:(ML;;NW;;;S-1-16-8448)"), Some(0x2100));
        assert_eq!(
            parse_label("O:S-1-5-21-1D:P(A;;FA;;;S-1-5-21-1)S:(ML;;NWNR;;;HI)"),
            Some(HIGH_INTEGRITY)
        );
        assert_eq!(parse_label("S:"), None);
        assert_eq!(parse_label(""), None);
        assert_eq!(parse_label("S:(ML;;NW;;;"), None);
        assert_eq!(parse_label("D:P(A;;FA;;;S-1-5-21-1)"), None);
        assert_eq!(integrity_rid("S-1-16-12288"), Some(HIGH_INTEGRITY));
        assert_eq!(integrity_rid("S-1-16-x"), None);
        assert_eq!(integrity_rid("S-1-5-21-1"), None);
        for level in [
            LOW_INTEGRITY,
            MEDIUM_INTEGRITY,
            HIGH_INTEGRITY,
            SYSTEM_INTEGRITY,
            0x2100,
        ] {
            let text = format!("S:(ML;;NWNR;;;{})", label(level));
            assert_eq!(parse_label(&text), Some(level), "{text}");
        }
        assert_eq!(label(MEDIUM_INTEGRITY), "ME");
        assert_eq!(label(0x2100), "S-1-16-8448");
    }
}
