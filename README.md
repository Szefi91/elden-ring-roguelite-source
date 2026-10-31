# Elden Ring Roguelite — source review snapshot 0.1.10

This repository contains the Rust implementation used for the Elden Ring Roguelite 0.1.10 release, prepared for source review.

## Build

Install Rust stable, the `x86_64-pc-windows-msvc` Rust target, Visual Studio C++ Build Tools (MSVC), and the Windows SDK. Then run from the repository root:

```powershell
cargo build --release --target x86_64-pc-windows-msvc --lib
```

## Asset note

This source snapshot intentionally excludes Elden Ring game files and game-extracted resources, including item icon PNGs, game UI sprites, the extracted item database, and the released binary. Those assets are not redistributed here. The released DLL embeds those resources; to reproduce the release build, the files referenced by `include_bytes!` and the generated game-data inputs must be supplied locally from a legally owned game installation. The relevant expected paths are under `refs/card-pools.json`, `refs/item-ikonok/`, `refs/kartya-dizajn/`, and `refs/fonts/`.

The source code can be inspected without those files. The build command above requires the omitted resource inputs to be restored first; the output will not match the public release until they are supplied.

No save files, logs, backups, or compiled `target/` output are included. The project source is shared for review; no separate reuse license is granted by this snapshot. Third-party crates retain their own licenses.
