//! Injector: loads er_roguelite.dll into a running (offline, EAC-disabled) Elden Ring.
//!
//! Usage:  er-inject.exe [process_name] [dll_path]
//! Defaults: process "eldenring.exe", dll the release build next to this exe's target.
//! ONLY run against an ERSC/offline launch -- never an online, EAC-protected instance.

use std::path::PathBuf;

use hudhook::inject::Process;

fn main() {
    let mut args = std::env::args().skip(1);
    let name = args.next().unwrap_or_else(|| "eldenring.exe".to_string());
    let dll: PathBuf = args
        .next()
        .unwrap_or_else(|| "er_roguelite.dll".to_string())
        .into();

    println!("[er-inject] target process: {name}");
    println!("[er-inject] dll: {}", dll.display());

    let process = match Process::by_name(&name) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[er-inject] could not find '{name}' running: {e:?}");
            eprintln!("[er-inject] launch Elden Ring (offline / Seamless Co-op) first.");
            std::process::exit(1);
        }
    };

    match process.inject(dll) {
        Ok(()) => println!("[er-inject] injected OK -- look for the overlay window in-game."),
        Err(e) => {
            eprintln!("[er-inject] inject failed: {e:?}");
            std::process::exit(2);
        }
    }
}
