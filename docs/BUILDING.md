# Building Outh

Outh is a Rust workspace:

- `crates/outh-core` — platform-independent library (protocol, auth, config,
  upload engine). Builds and tests anywhere Rust runs.
- `crates/outh-app` — the Windows desktop app on windows-reactor (WinUI 3),
  **self-contained**: `build.rs` calls
  `windows_reactor_setup::as_self_contained()`, which stages the Windows App
  Runtime into the build output.

## Requirements (Windows laptop)

- Rust stable, MSVC target (`x86_64-pc-windows-msvc`). windows-rs 0.100
  needs Rust ≥ 1.95; the laptop's stable toolchain is 1.96.
- Visual Studio 2026 with the MSVC C++ toolchain (the linker and Windows
  SDK come from there). A Rust-only install is not enough.
- Network access on the **first** app build: reactor-setup downloads the
  Windows App Runtime (WASDK 2.5.1) packages from NuGet for self-contained
  staging. Later builds reuse the cache.

## ⚠️ Laptop quirk: the `~/.cargo/bin` shims are broken

On Manish's laptop the rustup proxy binaries in `%USERPROFILE%\.cargo\bin`
(`cargo.exe`, `rustc.exe`) are **0-byte stubs**. A plain `cargo build`
spawns the stub `rustc` and fails with:

```
error: could not execute `rustc` ... os error 448 (untrusted mount point)
```

This is not a repo problem — it reproduces with a trivial `cargo new`
project. The real toolchain under `.rustup` is fine. Until the proxies are
repaired (e.g. `rustup self update` / reinstall), set `RUSTC` to the real
`rustc.exe` for every build, or invoke cargo from the toolchain's `bin`
directory:

```powershell
$env:RUSTC = "$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin\rustc.exe"
cargo build --release
cargo test -p outh-core
```

Equivalently, put the toolchain bin directory first on `PATH` for the
session:

```powershell
$env:PATH = "$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin;$env:PATH"
```

On GitHub Actions runners the standard rustup proxies work — CI does not
need this workaround (see `.github/workflows/build.yml`).

## Common commands

```powershell
cargo test -p outh-core          # library tests incl. protocol byte-fixtures (no network, no Windows APIs)
cargo build                      # debug build of the whole workspace (app build stages the App Runtime)
cargo build --release            # release build (thin LTO)
cargo run -p outh-app            # run the app (framework: self-contained staging)
cargo fmt --all                  # format (CI checks this)
cargo clippy -p outh-core        # lint the library
```

## Notes

- `cargo test -p outh-core` is the fast gate: it includes the protobuf
  byte-fixture tests that pin Outh's wire format against gotohp's Go
  implementation (fixtures in `crates/outh-core/tests/fixtures/golden.txt`,
  generator in `tools/gen-fixtures`).
- The app only runs on Windows (windows-reactor drives real WinUI 3
  windows); core tests also pass on the `windows-latest` CI runner.
- Self-contained vs framework-dependent is a build-time choice made by
  `windows-reactor-setup` in `outh-app/build.rs`; the app code is identical
  either way. Outh ships self-contained so target machines do not need the
  Windows App Runtime preinstalled.
