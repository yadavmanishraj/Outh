# Outh

A Google Photos desktop uploader for Windows, written in Rust on
[windows-reactor](https://github.com/microsoft/windows-rs) (WinUI 3) — a reimplementation of
[xob0t/gotohp](https://github.com/xob0t/gotohp).

Outh talks to the same private Google Photos upload protocol as gotohp (it does not use the
official Google Photos Library API): sign in via Google's Embedded Setup flow, then upload with
hash-based deduplication, optional album assignment, and Apple Live Photo pairing.

> **Honest labelling:** by default uploads claim a Pixel XL device identity (the legacy Pixel
> storage exemption), so they may not count toward your Google storage quota. This is client
> impersonation of an unofficial protocol — it can stop working at any time and may affect your
> account. Use the "Use quota" setting if you prefer uploads to count normally.

## Build

Requires Rust (stable, MSVC target) on Windows:

```powershell
cargo build --release
cargo test -p outh-core
```

## Layout

- `crates/outh-core` — protocol, auth, config, and the upload engine (port of gotohp's `core`)
- `crates/outh-app` — the windows-reactor (WinUI 3) desktop app, self-contained

See `CONTRACT.md` for the design contract and `docs/` for study notes.
