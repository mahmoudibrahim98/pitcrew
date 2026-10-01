//! Reports from the scripts PitCrew runs on a machine: `key=value` lines between two markers
//! that carry a random tag for each call, so neither a login banner nor a value the machine
//! prints can fake them.

use std::collections::HashMap;

/// The `key=value` lines between `@@pitcrew-<name>-begin-<tag>` and
/// `@@pitcrew-<name>-end-<tag>`, keys and values trimmed. Lines before the begin marker (login
/// banners) and after the end marker are ignored, as are lines without `=`.
///
/// # Errors
/// A marker is missing, or a key appears twice (a value with a newline in it must not stand in
/// for a later key). The message names the problem and quotes the start of the output.
pub(crate) fn parse<'a>(
    stdout: &'a str,
    name: &str,
    tag: &str,
) -> Result<HashMap<&'a str, &'a str>, String> {
    let begin = format!("@@pitcrew-{name}-begin-{tag}");
    let end = format!("@@pitcrew-{name}-end-{tag}");
    let unexpected = |why: &str| {
        let head: String = stdout.chars().take(200).collect();
        format!("{why}, in {head:?}")
    };
    let mut lines = stdout.lines().map(|l| l.trim_end_matches('\r'));
    if !lines.any(|l| l.trim() == begin) {
        return Err(unexpected(&format!("no {name} report")));
    }
    let mut values = HashMap::new();
    for line in lines {
        if line.trim() == end {
            return Ok(values);
        }
        if let Some((key, value)) = line.split_once('=')
            && values.insert(key.trim(), value.trim()).is_some()
        {
            return Err(unexpected(&format!("{:?} reported twice", key.trim())));
        }
    }
    Err(unexpected(&format!("the {name} report was cut off")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_need_both_markers_once() {
        let ok =
            "banner\n@@pitcrew-x-begin-t1\na=1\nb = two=2 \r\nnoise\n@@pitcrew-x-end-t1\nc=3\n";
        let values = parse(ok, "x", "t1").unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values["a"], "1");
        assert_eq!(values["b"], "two=2");
        assert!(
            parse("a=1\n", "x", "t1")
                .unwrap_err()
                .contains("no x report")
        );
        let cut = parse("@@pitcrew-x-begin-t1\na=1\n", "x", "t1").unwrap_err();
        assert!(cut.contains("cut off"), "{cut}");
        let twice = parse(
            "@@pitcrew-x-begin-t1\na=1\na=2\n@@pitcrew-x-end-t1\n",
            "x",
            "t1",
        );
        assert!(twice.unwrap_err().contains("twice"));
        // Another call's markers, or another script's, do not count.
        assert!(parse("@@pitcrew-x-begin-t2\n@@pitcrew-x-end-t2\n", "x", "t1").is_err());
        assert!(parse("@@pitcrew-y-begin-t1\n@@pitcrew-y-end-t1\n", "x", "t1").is_err());
    }
}
