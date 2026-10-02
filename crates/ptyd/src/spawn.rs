//! Checking a `start` request and turning it into a command: argv only, never a shell command
//! line.
//!
//! - The program is found as a file: a path (relative to the working directory), else a name
//!   on the request's `PATH` (else ptyd's own), absolute entries only, so a name is never a
//!   shell builtin and the working directory never decides what runs. On Windows, `PATHEXT`
//!   extensions are tried too.
//! - **Windows names.** A program whose name ends in a dot or a space, or names a stream
//!   (`:` after the drive), is refused: Windows would run another file than the name shows.
//! - **Windows batch files** (`.bat`, `.cmd`, such as npm's shims) run through `cmd.exe`, which
//!   parses their command line again with its own rules; an argument with a character
//!   `cmd.exe` treats specially (`" % ! ^ & | < > ( )` or a control character), or such a
//!   character in the batch file's path (`( )` allowed there once the path is quoted for a
//!   space), is refused rather than risk running something else.
//! - The working directory must be absolute and exist; variable names must be names (POSIX
//!   names on Unix; on Windows, no `=`) and nothing may hold a NUL.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use portable_pty::CommandBuilder;

/// Arguments a program may get, at most.
const MAX_ARGS: usize = 4096;
/// Variables a request may set, at most.
const MAX_ENV: usize = 4096;
/// Names are cut to this many characters.
const MAX_NAME: usize = 100;

/// A checked `start` request, ready to run.
#[derive(Debug)]
pub(crate) struct Launch {
    /// The program, as found.
    pub(crate) program: PathBuf,
    pub(crate) command: CommandBuilder,
    /// The name for people: control characters made spaces, at most 100 characters.
    pub(crate) name: String,
}

/// Checks a request and builds its command. The error is for people.
pub(crate) fn prepare(
    argv: &[String],
    cwd: &str,
    env: &[(String, String)],
    name: &str,
) -> Result<Launch, String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("no program was given".into());
    };
    if program.is_empty() {
        return Err("the program is empty".into());
    }
    if program.starts_with('-') {
        return Err("a program name may not start with '-'".into());
    }
    if args.len() > MAX_ARGS {
        return Err(format!("more than {MAX_ARGS} arguments"));
    }
    if env.len() > MAX_ENV {
        return Err(format!("more than {MAX_ENV} variables"));
    }
    if argv.iter().any(|a| a.contains('\0')) || cwd.contains('\0') {
        return Err("an argument holds a NUL".into());
    }
    if let Some((key, _)) = env
        .iter()
        .find(|(key, value)| !is_env_name(key) || value.contains('\0'))
    {
        return Err(format!(
            "{key:?} is not a variable name, or its value holds a NUL"
        ));
    }
    let dir = Path::new(cwd);
    if !dir.is_absolute() {
        return Err(format!("the working directory {cwd:?} is not absolute"));
    }
    if !dir.is_dir() {
        return Err(format!("the working directory {cwd:?} does not exist"));
    }
    let path = env
        .iter()
        .rev()
        .find(|(key, _)| same_name(key, "PATH"))
        .map(|(_, value)| OsString::from(value))
        .or_else(|| std::env::var_os("PATH"));
    let found = find(program, path.as_deref(), dir)
        .ok_or_else(|| format!("{program:?} is not a program file on PATH"))?;
    if cfg!(windows) {
        check_windows_program(&found, args)?;
    }
    let mut command = CommandBuilder::new(&found);
    command.args(args);
    command.cwd(dir);
    // What vt100 emulates, unless the request says otherwise.
    #[cfg(unix)]
    command.env("TERM", "xterm-256color");
    for (key, value) in env {
        command.env(key, value);
    }
    Ok(Launch {
        program: found,
        command,
        name: display_name(name),
    })
}

/// A name for people: control characters become spaces, at most 100 characters, trimmed.
pub(crate) fn display_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_NAME)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn same_name(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// A variable name: on Unix a POSIX name; on Windows anything without `=` or a control.
fn is_env_name(name: &str) -> bool {
    if cfg!(windows) {
        !name.is_empty() && !name.contains('=') && !name.chars().any(char::is_control)
    } else {
        let mut bytes = name.bytes();
        bytes
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
    }
}

/// The program file `program` names: itself if it holds a path separator (relative to `cwd`),
/// else the first match in `path`'s absolute directories.
pub(crate) fn find(program: &str, path: Option<&std::ffi::OsStr>, cwd: &Path) -> Option<PathBuf> {
    let has_separator = program.contains('/') || (cfg!(windows) && program.contains('\\'));
    if has_separator {
        return candidates(&cwd.join(program)).find(|c| is_program(c));
    }
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| candidates(&dir.join(program)).collect::<Vec<_>>())
        .find(|c| is_program(c))
}

/// The files a program path may mean: itself; on Windows, itself only if it has an extension,
/// then with each `PATHEXT` extension, as Windows looks for programs.
fn candidates(path: &Path) -> impl Iterator<Item = PathBuf> {
    let mut all = Vec::new();
    if !cfg!(windows) || path.extension().is_some() {
        all.push(path.to_owned());
    }
    if cfg!(windows) {
        let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        for ext in extensions
            .split(';')
            .filter(|e| e.starts_with('.') && e.len() > 1)
        {
            let mut with = path.as_os_str().to_owned();
            with.push(ext);
            all.push(PathBuf::from(with));
        }
    }
    all.into_iter()
}

fn is_program(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            meta.is_file()
        }
    })
}

/// Refuses, on Windows, a program path whose name Windows would read differently than it looks,
/// and a batch file with an argument (or a path) `cmd.exe` would act on.
///
/// - Windows drops trailing dots and spaces from a file name, and `name:stream` names a file's
///   stream: `t.cmd.`, `t.cmd ` and `t.cmd::$DATA` all run `t.cmd`, through `cmd.exe`, while
///   their extension does not look like `.cmd` (the CVE-2024-43402 class). Such names are
///   refused outright, so what is left is judged by the extension Windows uses.
/// - A batch file's command line is parsed again by `cmd.exe`: every argument, and the path
///   itself, must be free of `" % ! ^ & | < >` and control characters. `( )` are refused in
///   arguments, and in the path unless it holds a space (then it is quoted, and they are
///   literal inside quotes).
fn check_windows_program(found: &Path, args: &[String]) -> Result<(), String> {
    let shown = found.display();
    if let Some(why) = misleading_name(found) {
        return Err(format!("{shown}: {why}"));
    }
    if !is_batch(found) {
        return Ok(());
    }
    if let Some(arg) = args.iter().find(|a| !cmd_safe(a)) {
        return Err(format!(
            "{shown} is a batch file, which cmd.exe runs, and the argument {arg:?} holds a \
             character cmd.exe would act on"
        ));
    }
    if !batch_path_safe(&found.to_string_lossy()) {
        return Err(format!(
            "{shown} is a batch file, which cmd.exe runs, and its path holds a character \
             cmd.exe would act on"
        ));
    }
    Ok(())
}

/// Why Windows would read this path's name as something else: a stream (`:` after the drive),
/// or a trailing dot or space on the file name.
fn misleading_name(path: &Path) -> Option<&'static str> {
    let full = path.to_string_lossy();
    let rest = full
        .strip_prefix(r"\\?\")
        .or_else(|| full.strip_prefix(r"\\.\"))
        .unwrap_or(&full);
    let rest = match rest.as_bytes() {
        [drive, b':', ..] if drive.is_ascii_alphabetic() => &rest[2..],
        _ => rest,
    };
    if rest.contains(':') {
        return Some("a name with ':' names a file's stream, which is refused");
    }
    let name = path.file_name()?.to_string_lossy();
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("a name ending in a dot or a space is refused (Windows drops them)");
    }
    None
}

/// A `.bat` or `.cmd` file, which Windows runs through `cmd.exe`.
fn is_batch(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("bat") || e.eq_ignore_ascii_case("cmd"))
}

/// A character `cmd.exe` may act on even inside quotes, or in a value it parses again.
fn cmd_special(c: char) -> bool {
    c.is_control() || matches!(c, '"' | '%' | '!' | '^' | '&' | '|' | '<' | '>')
}

/// An argument `cmd.exe` passes on unchanged once quoted.
fn cmd_safe(arg: &str) -> bool {
    !arg.chars()
        .any(|c| cmd_special(c) || matches!(c, '(' | ')'))
}

/// A batch file's path `cmd.exe` takes as it is. portable-pty quotes it when it holds a space,
/// and `( )` are literal inside quotes.
fn batch_path_safe(path: &str) -> bool {
    let quoted = path.contains(' ');
    !path
        .chars()
        .any(|c| cmd_special(c) || (!quoted && matches!(c, '(' | ')')))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn requests_are_checked() {
        let tmp = std::env::temp_dir();
        let cwd = tmp.to_str().expect("utf-8 temp dir");
        let missing = tmp.join("no-such-dir-pitcrew");
        let missing = missing.to_str().expect("utf-8");
        for (argv, cwd, env, why) in [
            (strings(&[]), cwd, vec![], "no program"),
            (strings(&[""]), cwd, vec![], "empty"),
            (strings(&["-x"]), cwd, vec![], "may not start"),
            (strings(&["sh", "a\0b"]), cwd, vec![], "NUL"),
            (strings(&["sh"]), "relative/dir", vec![], "not absolute"),
            (strings(&["sh"]), missing, vec![], "does not exist"),
            (
                strings(&["sh"]),
                cwd,
                vec![("A=B".to_owned(), "x".to_owned())],
                "variable name",
            ),
            (
                strings(&["no-such-program-pitcrew"]),
                cwd,
                vec![],
                "not a program file",
            ),
        ] {
            let got = prepare(&argv, cwd, &env, "n").expect_err(why);
            assert!(got.contains(why), "{argv:?}: {got}");
        }
        assert_eq!(display_name(" agent\n%exit\t "), "agent %exit");
        assert_eq!(display_name(&"n".repeat(300)).len(), 100);
    }

    #[cfg(unix)]
    #[test]
    fn programs_are_files_never_builtins_or_relative_path_entries() {
        let path = OsString::from("relative/bin::/usr/bin:/bin");
        let sh = find("sh", Some(&path), Path::new("/")).expect("sh");
        assert!(sh.is_absolute() && sh.ends_with("sh"), "{}", sh.display());
        for builtin in ["eval", "exec", "trap", "cd"] {
            assert_eq!(
                find(builtin, Some(&path), Path::new("/")),
                None,
                "{builtin}"
            );
        }
        assert_eq!(find("sh", None, Path::new("/")), None);
        assert_eq!(
            find("bin/sh", Some(&path), Path::new("/")),
            Some(PathBuf::from("/bin/sh"))
        );
        assert_eq!(find("/tmp", Some(&path), Path::new("/")), None);
        // The request's PATH wins over ptyd's own.
        let launch = prepare(
            &strings(&["sh", "-c", "true"]),
            "/",
            &[("PATH".to_owned(), "/bin:/usr/bin".to_owned())],
            "x",
        )
        .expect("prepare");
        assert!(launch.program.starts_with("/bin") || launch.program.starts_with("/usr/bin"));
        assert_eq!(
            launch.command.get_env("TERM").and_then(|v| v.to_str()),
            Some("xterm-256color")
        );
        let custom = prepare(
            &strings(&["sh"]),
            "/",
            &[("TERM".to_owned(), "dumb".to_owned())],
            "x",
        )
        .expect("prepare");
        assert_eq!(
            custom.command.get_env("TERM").and_then(|v| v.to_str()),
            Some("dumb")
        );
    }

    #[test]
    fn batch_file_arguments_must_be_cmd_safe() {
        assert!(is_batch(Path::new(r"C:\npm\claude.CMD")));
        assert!(is_batch(Path::new("x.bat")));
        assert!(!is_batch(Path::new("claude.exe")));
        for safe in [
            "--model",
            "opus",
            "C:\\work dir\\x",
            "it's fine",
            "a=b,c.d:e@f+g?",
        ] {
            assert!(cmd_safe(safe), "{safe}");
        }
        for unsafe_arg in [
            "\"&calc", "%PATH%", "a&b", "a|b", "x>y", "(x)", "!x!", "^", "a\nb",
        ] {
            assert!(!cmd_safe(unsafe_arg), "{unsafe_arg}");
        }
    }

    #[test]
    fn names_windows_reads_differently_are_refused() {
        for misleading in [
            r"C:\npm\claude.cmd.",
            r"C:\npm\claude.cmd ",
            r"C:\npm\claude.cmd. . ",
            r"C:\npm\claude.cmd::$DATA",
            r"C:\npm\claude.cmd:x",
            r"\\?\C:\npm\claude.cmd:x",
            r"C:\npm:dir\claude.cmd",
            r"relative.cmd:x",
        ] {
            assert!(
                misleading_name(Path::new(misleading)).is_some(),
                "{misleading}"
            );
        }
        for plain in [
            r"C:\npm\claude.cmd",
            r"\\?\C:\Program Files\nodejs\node.exe",
            r"\\.\C:\x\y.exe",
            r"D:\a.b\c",
            r"C:\x\.hidden",
        ] {
            assert_eq!(misleading_name(Path::new(plain)), None, "{plain}");
        }
        // A batch file's own path is judged too; ( ) only once quoted for its space.
        assert!(batch_path_safe(
            r"C:\Users\a\AppData\Roaming\npm\claude.cmd"
        ));
        assert!(batch_path_safe(r"C:\Program Files (x86)\x\t.cmd"));
        for bad in [
            r"C:\a&b\t.cmd",
            r"C:\(x)\t.cmd",
            r"C:\100%\t.cmd",
            r"C:\a b\x^y\t.cmd",
            r"C:\a b\x!y!\t.cmd",
        ] {
            assert!(!batch_path_safe(bad), "{bad}");
        }
        // Applied together: a plain exe passes whatever its arguments; a batch file does not.
        let hostile = strings(&["&echo x>pwned"]);
        assert!(check_windows_program(Path::new(r"C:\x\tool.exe"), &hostile).is_ok());
        for refused in [r"C:\x\t.cmd", r"C:\x\t.BAT", r"C:\x\t.cmd.", r"C:\x\t.cmd "] {
            let why = check_windows_program(Path::new(refused), &hostile).expect_err(refused);
            assert!(why.contains("cmd.exe") || why.contains("refused"), "{why}");
        }
        assert!(check_windows_program(Path::new(r"C:\x\t.cmd"), &strings(&["--resume"])).is_ok());
        assert!(check_windows_program(Path::new(r"C:\a&b\t.cmd"), &[]).is_err());
    }
}
