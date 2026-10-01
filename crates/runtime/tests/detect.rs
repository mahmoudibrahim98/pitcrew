use pitcrew_runtime::detect::{DetectError, TmuxVersion, VersionKind, detect_tmux};

#[test]
fn version_flavors_and_compatibility_floor() {
    for (text, kind, major, minor, suffix, supported) in [
        ("tmux 2.7", VersionKind::Release, 2, 7, "", false),
        ("tmux 2.9a", VersionKind::Release, 2, 9, "a", false),
        ("tmux 3.0", VersionKind::Release, 3, 0, "", false),
        ("tmux 3.1c", VersionKind::Release, 3, 1, "c", false),
        ("tmux 3.2\n", VersionKind::Release, 3, 2, "", true),
        ("tmux 3.3a\r\n", VersionKind::Release, 3, 3, "a", true),
        ("tmux next-3.4", VersionKind::Next, 3, 4, "", true),
        ("tmux next-3.1", VersionKind::Next, 3, 1, "", false),
        ("tmux openbsd-7.4", VersionKind::OpenBsd, 7, 4, "", true),
        ("tmux openbsd-6.8", VersionKind::OpenBsd, 6, 8, "", false),
        ("tmux openbsd-6.9", VersionKind::OpenBsd, 6, 9, "", true),
        (
            "tmux openbsd-7.4-current",
            VersionKind::OpenBsd,
            7,
            4,
            "-current",
            true,
        ),
        ("tmux 3.10", VersionKind::Release, 3, 10, "", true),
        ("tmux 10.0", VersionKind::Release, 10, 0, "", true),
    ] {
        let version: TmuxVersion = text.parse().expect("version");
        assert_eq!(
            version,
            TmuxVersion {
                kind,
                major,
                minor,
                suffix: suffix.into()
            }
        );
        assert_eq!(version.is_supported(), supported, "{text}");
        assert_eq!(format!("tmux {version}"), text.trim());
    }
    for invalid in [
        "",
        "3.2",
        "tmux next",
        "tmux 3",
        "tmux 3.x",
        "tmux -3.2",
        "tmux 3.2 garbage",
        "tmux 3.2;",
        "tmux 3.2\ntmux 2.7",
        "tmux 4294967296.2",
        "tmux openbsd-3.2x",
    ] {
        assert!(
            matches!(
                invalid.parse::<TmuxVersion>(),
                Err(DetectError::InvalidVersion(_))
            ),
            "{invalid}"
        );
    }
}

#[test]
fn missing_executable_is_a_fallback_error() {
    let missing = std::env::temp_dir().join(format!("pitcrew-missing-{}/tmux", std::process::id()));
    assert!(
        matches!(detect_tmux(missing), Err(DetectError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
    );
}

#[cfg(unix)]
mod probe {
    use super::*;
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    struct Probe(PathBuf);

    impl Probe {
        fn new(script: &str) -> Self {
            let id = RandomState::new().hash_one(std::process::id());
            // Spaces and shell punctuation verify that path is executed as argv.
            let path = std::env::temp_dir().join(format!("pitcrew-probe {id:x}; literal"));
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("probe file");
            file.write_all(script.as_bytes()).expect("write probe");
            file.set_permissions(std::fs::Permissions::from_mode(0o700))
                .expect("executable");
            Self(path)
        }
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(format!("{}.pid", self.0.display()));
        }
    }

    #[test]
    fn hanging_probe_times_out_and_is_reaped() {
        let probe = Probe::new("#!/bin/sh\nprintf '%s' \"$$\" > \"$0.pid\"\nexec sleep 10\n");
        let started = std::time::Instant::now();
        assert!(matches!(detect_tmux(&probe.0), Err(DetectError::TimedOut)));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let pid = std::fs::read_to_string(format!("{}.pid", probe.0.display())).expect("probe pid");
        let alive = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("check child process");
        assert!(!alive.success(), "timed-out child still exists");
    }

    #[test]
    fn executes_version_probe_and_rejects_old_or_invalid_results() {
        for output in [
            "tmux 2.7",
            "tmux 3.1c",
            "tmux 3.2a",
            "tmux next-3.4",
            "tmux openbsd-7.4",
        ] {
            let probe = Probe::new(&format!(
                "#!/bin/sh\n[ \"$#\" = 1 ] && [ \"$1\" = -V ] || exit 99\nprintf '%s\\n' '{output}'\n"
            ));
            let expected: TmuxVersion = output.parse().expect("parse");
            if expected.is_supported() {
                assert_eq!(detect_tmux(&probe.0).expect("supported"), expected);
            } else {
                assert!(
                    matches!(detect_tmux(&probe.0), Err(DetectError::Unsupported(version)) if version == expected)
                );
            }
        }
        let probe = Probe::new("#!/bin/sh\nprintf 'tmux 3.2\\n'\nprintf broken >&2\nexit 7\n");
        assert!(matches!(
            detect_tmux(&probe.0),
            Err(DetectError::ProbeFailed { code: Some(7), .. })
        ));
        let probe = Probe::new("#!/bin/sh\nprintf 'not tmux\\n'\n");
        assert!(matches!(
            detect_tmux(&probe.0),
            Err(DetectError::InvalidVersion(_))
        ));
        let probe = Probe::new("#!/bin/sh\nprintf '\\377'\n");
        assert!(matches!(
            detect_tmux(&probe.0),
            Err(DetectError::InvalidVersion(_))
        ));
    }
}
