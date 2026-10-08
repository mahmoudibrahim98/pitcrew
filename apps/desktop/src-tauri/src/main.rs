//! The PitCrew desktop app. Everything is in the library; see `lib.rs`.

// No console window next to the app on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    // `--check-layout` alone: report what is next to the app and exit, with no window.
    let mut args = std::env::args_os().skip(1);
    if let (Some(arg), None) = (args.next(), args.next())
        && arg == pitcrew_desktop::portable::CHECK_LAYOUT
    {
        return pitcrew_desktop::portable::check_layout_main();
    }
    pitcrew_desktop::run()
}
