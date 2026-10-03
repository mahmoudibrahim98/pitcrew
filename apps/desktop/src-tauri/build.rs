//! Tauri's build step: the app's context (config, icons, embedded UI in release builds) and its
//! access-control list. Listing the commands here gives the app an ACL manifest, so each command
//! is callable only from windows whose capability names it (`capabilities/main.json`).

fn main() {
    let manifest = tauri_build::AppManifest::new().commands(&[
        "gateway_workspaces",
        "gateway_local_host",
        "gateway_request",
        "gateway_socket_open",
        "gateway_socket_send",
        "gateway_socket_close",
        "gateway_ssh_hosts",
        "gateway_wsl_distros",
        "gateway_remote_probe",
        "gateway_remote_plan",
        "gateway_remote_add",
        "gateway_remote_cancel",
        "gateway_workspace_retry",
        "gateway_workspace_remove",
        "gateway_prompt_reply",
    ]);
    let attributes = tauri_build::Attributes::new().app_manifest(manifest);
    if let Err(e) = tauri_build::try_build(attributes) {
        eprintln!("tauri-build failed: {e:#}");
        std::process::exit(1);
    }
}
