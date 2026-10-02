//! What `/proc` says about a process: its memory, the CPU it has used, and per thread. Linux
//! only; on any other system every reader returns `None`.

use std::fs;

/// Memory figures of a process, in KiB, from `/proc/<pid>/status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    /// `VmRSS`: resident now.
    pub rss: u64,
    /// `VmHWM`: the most it has been resident ("high water mark").
    pub peak: u64,
    /// `RssAnon`: the resident part that is heap and stacks.
    pub anon: u64,
    /// `RssFile`: the resident part that is mapped files (the binary, mapped databases).
    pub file: u64,
}

/// A process's memory, or `None` if it is gone or the system has no `/proc`.
#[must_use]
pub fn memory(pid: u32) -> Option<Memory> {
    parse_status(&fs::read_to_string(format!("/proc/{pid}/status")).ok()?)
}

/// The figures in the text of a `/proc/<pid>/status` file.
#[must_use]
pub fn parse_status(text: &str) -> Option<Memory> {
    let field = |name: &str| -> Option<u64> {
        text.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(':'))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    Some(Memory {
        rss: field("VmRSS")?,
        peak: field("VmHWM")?,
        anon: field("RssAnon").unwrap_or(0),
        file: field("RssFile").unwrap_or(0),
    })
}

/// User plus system CPU time of a process, in clock ticks (100 a second on Linux).
#[must_use]
pub fn cpu_ticks(pid: u32) -> Option<u64> {
    parse_stat(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?).map(|(_, t)| t)
}

/// The thread name and CPU ticks (user plus system) in the text of a `stat` file. The name is
/// in parentheses and may hold spaces and parentheses, so the fields are counted from the last
/// closing one.
#[must_use]
pub fn parse_stat(stat: &str) -> Option<(String, u64)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_owned();
    // After the name: state is field 3, utime and stime are fields 14 and 15.
    let fields: Vec<&str> = stat.get(close + 2..)?.split(' ').collect();
    let ticks = fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
    Some((name, ticks))
}

/// One thread of a process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    /// The thread id: threads started earlier have lower ones.
    pub tid: u32,
    /// Its name, cut to 15 characters by the kernel.
    pub name: String,
    /// CPU ticks it has used (user plus system).
    pub ticks: u64,
}

/// The threads of a process, busiest first. Threads of one name stay apart, so two workers of a
/// pool show as two entries.
#[must_use]
pub fn threads(pid: u32) -> Vec<Thread> {
    let Ok(dir) = fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    let mut all: Vec<Thread> = dir
        .flatten()
        .filter_map(|task| {
            let tid = task.file_name().to_str()?.parse().ok()?;
            let (name, ticks) = parse_stat(&fs::read_to_string(task.path().join("stat")).ok()?)?;
            Some(Thread { tid, name, ticks })
        })
        .collect();
    all.sort_by_key(|t| std::cmp::Reverse(t.ticks));
    all
}

/// Whether process `pid` still exists.
#[must_use]
pub fn alive(pid: u32) -> bool {
    fs::metadata(format!("/proc/{pid}")).is_ok()
}

/// The pids of processes whose command line contains `needle`. For checking that nothing is left
/// running; it never matches this process.
#[must_use]
pub fn find_by_cmdline(needle: &str) -> Vec<u32> {
    let me = std::process::id();
    let Ok(dir) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != me)
        .filter(|pid| {
            fs::read(format!("/proc/{pid}/cmdline"))
                .map(|raw| {
                    String::from_utf8_lossy(&raw)
                        .replace('\0', " ")
                        .contains(needle)
                })
                .unwrap_or(false)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_memory_lines() {
        let text = "Name:\tpitcrewd\nVmPeak:\t  900000 kB\nVmHWM:\t   61234 kB\nVmRSS:\t   55000 kB\n\
                    RssAnon:\t   40000 kB\nRssFile:\t   14000 kB\nRssShmem:\t       0 kB\n";
        assert_eq!(
            parse_status(text),
            Some(Memory {
                rss: 55_000,
                peak: 61_234,
                anon: 40_000,
                file: 14_000
            })
        );
        assert_eq!(parse_status("Name:\tx\n"), None);
    }

    #[test]
    fn counts_fields_after_the_name() {
        let stat = "123 (a (weird) name) S 1 2 3 4 5 6 7 8 9 10 111 222 0 0 20 0 1 0 5 6 7";
        assert_eq!(parse_stat(stat), Some(("a (weird) name".to_owned(), 333)));
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn reads_this_process() {
        if cfg!(target_os = "linux") {
            let me = std::process::id();
            let mem = memory(me).expect("our own memory");
            assert!(mem.rss > 0 && mem.peak >= mem.rss);
            assert!(cpu_ticks(me).is_some());
            assert!(!threads(me).is_empty());
            assert!(alive(me));
            assert!(!find_by_cmdline("definitely-not-a-command-line-xyzzy").contains(&me));
        }
    }
}
