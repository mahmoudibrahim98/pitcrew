//! A small unified diff between two whole-file texts, for `pitcrew hooks diff`. Config files are
//! tiny, so a plain `O(n*m)` longest-common-subsequence diff is fast enough and easy to check.

const CONTEXT: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

/// A unified diff of `before` (as `path`) against `after` (as `path`), or a one-line note when
/// they are identical.
#[must_use]
pub(crate) fn unified(path: &str, before: &str, after: &str) -> String {
    if before == after {
        return format!("(no change to {path})\n");
    }
    let a = split_lines(before);
    let b = split_lines(after);
    let ops = diff(&a, &b);
    render(path, &a, &b, &ops)
}

/// Lines, each keeping its own trailing `\n` so the original text can be told apart from one
/// missing a final newline.
fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = Vec::new();
    let mut start = 0;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            lines.push(&text[start..=i]);
            start = i + 1;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn diff(a: &[&str], b: &[&str]) -> Vec<Op> {
    let (n, m) = (a.len(), b.len());
    // dp[i][j] = length of the longest common subsequence of a[i..] and b[j..].
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(Op::Equal(i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(Op::Delete(i));
            i += 1;
        } else {
            ops.push(Op::Insert(j));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Delete(i));
        i += 1;
    }
    while j < m {
        ops.push(Op::Insert(j));
        j += 1;
    }
    ops
}

fn render(path: &str, a: &[&str], b: &[&str], ops: &[Op]) -> String {
    // Group changed regions (with `CONTEXT` lines of surrounding equal lines) into hunks.
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| !matches!(op, Op::Equal(..)))
        .map(|(k, _)| k)
        .collect();
    if changed.is_empty() {
        return format!("(no change to {path})\n");
    }
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    let mut start = changed[0].saturating_sub(CONTEXT);
    let mut end = (changed[0] + 1 + CONTEXT).min(ops.len());
    for &k in &changed[1..] {
        let lo = k.saturating_sub(CONTEXT);
        if lo <= end {
            end = (k + 1 + CONTEXT).min(ops.len());
        } else {
            hunks.push((start, end));
            start = lo;
            end = (k + 1 + CONTEXT).min(ops.len());
        }
    }
    hunks.push((start, end));

    let mut out = format!("--- {path}\n+++ {path}\n");
    for (start, end) in hunks {
        let slice = &ops[start..end];
        let (mut a_at, mut b_at) = (None, None);
        for op in slice {
            match *op {
                Op::Equal(ai, bi) => {
                    a_at.get_or_insert(ai);
                    b_at.get_or_insert(bi);
                }
                Op::Delete(ai) => {
                    a_at.get_or_insert(ai);
                }
                Op::Insert(bi) => {
                    b_at.get_or_insert(bi);
                }
            }
        }
        let a_count = slice
            .iter()
            .filter(|o| matches!(o, Op::Equal(..) | Op::Delete(_)))
            .count();
        let b_count = slice
            .iter()
            .filter(|o| matches!(o, Op::Equal(..) | Op::Insert(_)))
            .count();
        let a_start = a_at.unwrap_or(0);
        let b_start = b_at.unwrap_or(0);
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            a_start + 1,
            a_count,
            b_start + 1,
            b_count
        ));
        for op in slice {
            match *op {
                Op::Equal(ai, _) => out.push_str(&format!(" {}", ensure_nl(a[ai]))),
                Op::Delete(ai) => out.push_str(&format!("-{}", ensure_nl(a[ai]))),
                Op::Insert(bi) => out.push_str(&format!("+{}", ensure_nl(b[bi]))),
            }
        }
    }
    out
}

/// A line for display: always ends in a newline, even if the file's last line did not (git marks
/// that case separately; a trailing note is enough detail for a config-file diff).
fn ensure_nl(line: &str) -> String {
    if line.ends_with('\n') {
        line.to_owned()
    } else {
        format!("{line}\n\\ No newline at end of file\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_has_no_diff() {
        assert_eq!(unified("f", "a\nb\n", "a\nb\n"), "(no change to f)\n");
    }

    #[test]
    fn a_pure_insertion() {
        let out = unified("f", "a\nb\n", "a\nx\nb\n");
        assert!(out.contains("+x\n"), "{out}");
        assert!(out.contains(" a\n"), "{out}");
        assert!(out.contains(" b\n"), "{out}");
        assert!(!out.contains("-a\n"), "{out}");
    }

    #[test]
    fn a_replacement_in_the_middle() {
        let out = unified("f", "a\nb\nc\n", "a\nB\nc\n");
        assert!(out.contains("-b\n"), "{out}");
        assert!(out.contains("+B\n"), "{out}");
    }

    #[test]
    fn a_missing_final_newline_is_marked() {
        let out = unified("f", "a\n", "a\nb");
        assert!(out.contains("\\ No newline at end of file"), "{out}");
    }
}
