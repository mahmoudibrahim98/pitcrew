//! The PitCrew desktop app. Everything is in the library; see `lib.rs`.

// No console window next to the app on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    pitcrew_desktop::run()
}
