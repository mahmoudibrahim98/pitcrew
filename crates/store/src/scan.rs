//! Finds migration files in a directory. Standard library only, because `build.rs` includes this
//! file directly (`#[path]`) as well as the library.

use std::path::{Path, PathBuf};

/// A migration file found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The number, e.g. `101` for `0101_leases.sql`.
    pub version: u32,
    /// The name after the number, e.g. `leases`.
    pub name: String,
    /// The file.
    pub path: PathBuf,
}

/// Parses a file name. `Ok(None)` for files that are not SQL (such as a README), an error for SQL
/// files that do not follow `NNNN_<name>.sql`.
pub fn parse_file_name(file_name: &str) -> Result<Option<(u32, String)>, String> {
    let Some(stem) = file_name.strip_suffix(".sql") else {
        return Ok(None);
    };
    let bad = || {
        format!(
            "bad migration file name {file_name:?}: expected NNNN_<name>.sql, four digits then a \
             lowercase name of letters, digits and underscores"
        )
    };
    let (number, name) = stem.split_once('_').ok_or_else(bad)?;
    if number.len() != 4 || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let name_ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !name_ok {
        return Err(bad());
    }
    let version: u32 = number.parse().map_err(|_| bad())?;
    if version == 0 {
        return Err(format!(
            "bad migration file name {file_name:?}: 0000 is reserved"
        ));
    }
    Ok(Some((version, name.to_owned())))
}

/// Lists the migrations in `dir`, sorted by number. Rejects bad names and duplicate numbers.
pub fn scan_dir(dir: &Path) -> Result<Vec<Found>, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            return Err(format!("non-UTF-8 file name in {}", dir.display()));
        };
        if let Some((version, name)) = parse_file_name(file_name)? {
            found.push(Found {
                version,
                name,
                path,
            });
        }
    }
    found.sort_by_key(|f| f.version);
    for pair in found.windows(2) {
        if pair[0].version == pair[1].version {
            return Err(format!(
                "duplicate migration number {:04}: {} and {}",
                pair[0].version,
                pair[0].path.display(),
                pair[1].path.display()
            ));
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::parse_file_name;

    #[test]
    fn file_names() {
        assert_eq!(
            parse_file_name("0001_init.sql"),
            Ok(Some((1, "init".into())))
        );
        assert_eq!(
            parse_file_name("0203_task_links.sql"),
            Ok(Some((203, "task_links".into())))
        );
        assert_eq!(parse_file_name("README.md"), Ok(None));
        for bad in [
            "1_init.sql",
            "00001_init.sql",
            "0001-init.sql",
            "0001_.sql",
            "0001_Init.sql",
            "0001_in it.sql",
            "abcd_init.sql",
            "0000_zero.sql",
            "init.sql",
        ] {
            assert!(parse_file_name(bad).is_err(), "{bad} should be rejected");
        }
    }
}
