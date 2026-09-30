//! `GET /v1/host/info` and the local machine's facts.

use pitcrew_protocol::api::{HostInfo, HostRole};
use pitcrew_protocol::model::{MachineInfo, Scheduler};
use pitcrew_protocol::runner::Capability;
use std::path::Path;

/// Host info for this process: `pitcrewd` at `version`, the protocol range this build speaks, and
/// [`local_machine_info`].
#[must_use]
pub fn local_host_info(
    version: impl Into<String>,
    roles: Vec<HostRole>,
    capabilities: Vec<Capability>,
) -> HostInfo {
    HostInfo {
        name: "pitcrewd".to_owned(),
        version: version.into(),
        protocol: pitcrew_protocol::PROTOCOL_VERSION,
        protocol_min: pitcrew_protocol::PROTOCOL_MIN,
        roles,
        machine: local_machine_info(),
        capabilities,
    }
}

/// What can be learned about this machine cheaply and without privileges. Whether the home
/// directory is on a network filesystem is not detected here and is reported as `false`.
#[must_use]
pub fn local_machine_info() -> MachineInfo {
    MachineInfo {
        hostname: hostname(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        has_tmux: on_path("tmux"),
        scheduler: on_path("sbatch").then_some(Scheduler::Slurm),
        home_on_network_fs: false,
    }
}

fn hostname() -> String {
    let from_env = ["COMPUTERNAME", "HOSTNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok());
    let from_files = ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok());
    from_env
        .chain(from_files)
        .map(|name| name.trim().to_owned())
        .find(|name| !name.is_empty())
        .or_else(|| {
            let out = std::process::Command::new("hostname").output().ok()?;
            let name = String::from_utf8(out.stdout).ok()?.trim().to_owned();
            (out.status.success() && !name.is_empty()).then_some(name)
        })
        .unwrap_or_else(|| "localhost".to_owned())
}

fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let names: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    std::env::split_paths(&path).any(|dir| {
        names
            .iter()
            .any(|ext| is_file(&dir.join(format!("{program}{ext}"))))
    })
}

fn is_file(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| m.is_file())
}
