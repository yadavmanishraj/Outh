# Outh architecture — how it maps to gotohp

Outh is a Rust reimplementation of [gotohp](https://github.com/xob0t/gotohp)
(Go + Wails + Vue desktop Google Photos uploader, studied at HEAD `97a5dc0`)
on [windows-reactor](https://github.com/microsoft/windows-rs) (WinUI 3).
The design contract lives in `CONTRACT.md`; this document records the
mapping and the reasoning so future drift fixes land in the right place.

gotohp's shape made this port tractable: PR #109 upstream had just
extracted all backend logic into a UI-independent Go package `core/`
(~6,900 LOC) behind one 33-line seam (the `UploadReporter` interface).
Outh ports `core/` file by file into `crates/outh-core` and discards the
Wails/Vue layer entirely, recreating it as a reactor app in
`crates/outh-app`. Nothing about the wire protocol changes.

## Module map: Go → Rust

| gotohp (Go) | Outh (Rust) | Notes |
|---|---|---|
| `core/api.go` | `outh-core/src/api.rs` (`PhotosClient`) | Bearer exchange, GetUploadToken, HashCheck, Scotty PUT, commit, albums. Device fingerprint + magic headers copied with source citations. |
| `core/upload.go` + `core/album.go` | `outh-core/src/upload.rs` (`UploadManager`) | Worker pool = `std::thread` + `std::sync::mpsc` (CONTRACT §7; no tokio). Album orchestration (named + per-folder AUTO) included. |
| `core/reporter.go` + `core/upload_options.go` | `outh-core/src/types.rs` (`UploadReporter`, `ThreadStatus`, `FileResult`, …) + `upload.rs` (`UploadOptions`) | The one presentation seam, kept 1:1: core knows nothing about the UI. The app implements the reporter by forwarding callbacks into reactor component messages. |
| `core/googleauth.go` | `outh-core/src/auth.rs` (`EmbeddedSetupAuth`) | Embedded Setup `oauth_token` → master token exchange, then the Photos credential template. Endpoints/client injectable for tests, mirroring Go's dependency struct. |
| credential string handling in `core/configmanager.go` / `core/api.go` | `outh-core/src/credential.rs` (`Credential`) | The `key=value&…` query-style string, parsed losslessly (unknown keys preserved — the bearer request replays it near-verbatim). Carries the `needs_token_binding` derivation. |
| `core/configmanager.go` | `outh-core/src/config.rs` (`Config`, `Preferences`, `Account`, `ConfigService`) | YAML/koanf → serde JSON (see decisions). Defaults must match Go: `upload_threads = 3`, `skip_incomplete_live_photos = true`, everything else zero. |
| `core/httpclient.go` + `core/progress_reader.go` | `outh-core/src/http.rs` | `ureq` 3 + rustls, synchronous. Counting-reader progress, throttled like Go (100 ms). Retry policy stays at the call sites (3 retries, backoff + jitter) because retryability differs per call (see protocol notes). |
| `.proto/*.proto` (10 files) + `generated/*.pb.go` | `outh-core/src/protocol.rs` + `outh-core/src/wire.rs` | Hand-rolled protobuf wire format; no prost/protobuf crate. Field numbers are the contract; several messages are fingerprint theatre (empty nested messages) whose bytes must be reproduced exactly. |
| `core/scotty_token.go` | `ScottyToken` in `protocol.rs` | Finalize token kept as opaque raw bytes, validated at wire level (field 1 == varint 2 exactly once, field 2 non-empty bytes exactly once). Never decode-and-re-encode: Live Photo commits embed the envelope byte-for-byte. |
| `core/create_media_items.go` | `outh-core/src/create_media_items.rs` | Live Photo `CreateMediaItems` blueprints, incl. the opaque ~700-byte `result_item_mask` (a base64 constant copied from captured official-client traffic). |
| `core/livephoto.go` + `core/livephoto_upload.go` | `outh-core/src/livephoto.rs` | Pairing by Apple content-identifier; still+video commit in ONE blueprint; reconcile path when the still is already remote (`RECONCILE_TYPE_PHODEO`). |
| `core/livephoto_metadata.go` | `outh-core/src/livephoto_metadata.rs` | Hand-rolled EXIF/MakerNote + QuickTime box parsers. Ported literally; do not substitute an EXIF library. |
| `core/filename_parser.go` | `outh-core/src/filename.rs` | 4 regex patterns for capture-time-from-filename; local time; 1990…next-year bounds. |
| `core/sha1calc.go` | `outh-core/src/sha1calc.rs` | Streaming SHA-1, 1 MiB buffer. SHA-1 is a server-demanded content identifier (dedup + `X-Goog-Hash`), not a security control — do not "upgrade" it. |
| `core/tokenbinding.go` + ADB extraction in `configmanager.go` | **not ported (v1)** | See decisions. Credentials that would need it are flagged `needs_token_binding` and badged in the UI. |
| `main.go`, `internal/gui/`, Vue frontend | `outh-app/src/main.rs` (reactor component tree) | Upload / Accounts / Settings sections. Upload runs via reactor `spawn_background`; no in-process event bus (that existed only for the webview boundary). |
| `internal/cli/` (cobra) | — | No CLI in v1 (GUI-only decision). Core is host-agnostic, so a CLI remains a thin future host, exactly as in Go. |

## The protocol on one page

Outh does **not** use the official Google Photos Library API (no OAuth 2,
no `photoslibrary.googleapis.com`). It impersonates the Android Photos app
(Pixel XL / Android 9 / API 28, Photos `clientVersionCode 49029607`, Cronet
user-agent) and speaks private protobuf-over-HTTPS:

**Auth.** Credentials are Android Play Services *master-token* credential
strings. Sign-in (Embedded Setup): the user copies the `oauth_token` cookie
from Google's Embedded Setup page; Outh POSTs it to
`android.clients.google.com/auth` (form-encoded, Play Services cert sig,
`droidguard_results=dummy123`) and receives a master token (`aas_et/…`),
which is wrapped in the fixed Photos credential template
(`googleauth.go buildGooglePhotosCredential`). Per session, the credential
is replayed to `android.googleapis.com/auth` for a bearer (`Auth` +
`Expiry`, cached in memory only). Redirects are never followed on auth
endpoints; response bodies are capped at 64 KiB; errors never echo tokens.
There is **no 401 handling** upstream — a dead master token means
re-capturing a credential.

**Upload, per file** (state machine: hashing → checking → uploading →
finalizing → completed / skipped / error):

1. Stream SHA-1 (1 MiB buffer).
2. **HashCheck** → `POST photosdata-pa.googleapis.com/6439526531001121323/5084965799730810217`
   with `HashCheck{1:{1:{sha1},2:{}}}`. If `RemoteMatches` returns a media
   key at `field1.field2.field2.media_key`, the file is **skipped with zero
   bytes uploaded** (unless ForceUpload).
3. **GetUploadToken** → `POST photos.googleapis.com/data/upload/uploadmedia/interactive`
   with `GetUploadToken{1:2, 2:2, 3:1, 4:3, 7:file_size}` and header
   `X-Goog-Hash: sha1=<base64>`. The session id arrives **only** in the
   `X-GUploader-UploadID` response header.
4. **Scotty upload** → a single chunked `PUT` to the same URL
   `?upload_id=<session>` with unknown content length (deliberately not
   known-length framing). The response is the Scotty finalize token
   (protobuf `{1:2, 2:opaque}`), preserved byte-for-byte. **No
   resumability**: a retry (max 3) re-streams the whole file from byte 0.
5. **Commit** → `POST …/6439526531001121323/16538846908252377752`
   (the `CreateMediaItems` RPC) with either the legacy `CommitUpload`
   message (token deconstructed into `field1{field1,field2}`, file name,
   SHA-1, mtime + magic `46000000`, quality, device block, trailer bytes
   `{1,3}`) or, for Live Photos, a `CreateMediaItems` blueprint. Commit
   requests carry the magic headers `x-goog-ext-173412678-bin: CgcIAhClARgC`
   and `x-goog-ext-174067345-bin: CgIIAg==`. A 2xx response with an
   unparseable body is **never retried** (`Error::CommitAmbiguous`) — the
   media may exist, and a retry would duplicate it.
6. Albums: **CreateAlbum** → `…/8386163679468898444`,
   **AddMediaToAlbum** → `…/484917746253879292`, same commit headers.

**"Unlimited" semantics.** Quality/policy/device are chosen together:
default = Pixel XL identity + storage policy 3 (the legacy Pixel
original-quality quota exemption) and `CommitUpload` quality 3; Saver =
Pixel 2 + policy 1 + quality 1; "Use quota" forces the device model to
Pixel 8 so uploads count normally. Capture time is file mtime, or a
filename-regex parse when that preference is on — there are no EXIF dates
anywhere in the protocol.

**Live Photos.** Still (HEIC/JPEG) and video (MOV) are paired by the Apple
content-identifier UUID (EXIF MakerNote tag 17 vs QuickTime metadata, both
hand-parsed). Both components upload through steps 3–4, then commit in one
blueprint (`live_photo_info` field 24 embedding the video token's *raw
envelope*). If only the still is already remote, a reconcile blueprint
(`RECONCILE_TYPE_PHODEO`) is used instead.

## Decisions and deviations from gotohp

Frozen in `CONTRACT.md`; the ones a future maintainer must not silently
undo:

1. **TLS verification is never disabled.** Go sets
   `InsecureSkipVerify = true` whenever a proxy is configured
   (`core/httpclient.go`) — a deliberate deviation: Outh validates
   certificates with a proxy too. A TLS-intercepting proxy needs an
   explicit future opt-in, not a silent default.
2. **Token binding is deferred (not in v1).** It is only needed by *some
   imported Android-harvested credentials* (`assertion_jwt` or
   `check_tb_upgrade_eligible` present, no `token_binding_alias` stored).
   Embedded Setup credentials never trigger it. Outh parses and flags such
   credentials (`needs_token_binding`) and badges them in the UI instead of
   pretending they will work. Implementing it later means: ES256 assertion
   JWT + hand-built Tink ECIES (ECDH + HKDF-SHA256 + AES-GCM) and the ADB
   `accounts_ce.db` alias extraction — see study RESEARCH_3/5.
3. **Credentials at rest go through `CredentialProtector`.** Go stores
   master tokens as plaintext YAML. Outh's core depends on a
   `CredentialProtector` trait; the app supplies a DPAPI-backed
   implementation on Windows (core tests use an identity protector).
   Master tokens are full-account bearer credentials — treat any config
   backup as a credential backup.
4. **Config format changed: YAML → JSON** (serde), at
   `%APPDATA%\Outh\config.json` (a portable `outh.config.json` next to the
   exe wins, mirroring Go's lookup order). Writes keep Go's atomic
   temp + rename discipline. `album_name` / `album_auto_mode` remain
   session-only, as upstream. Legacy gotohp YAML import is *not* in v1;
   the required migration cases are pinned in
   `crates/outh-core/tests/config_legacy_yaml.rs` (feature-gated) in case
   it is added.
5. **No WebView2 sign-in.** Hosting Embedded Setup in-app and harvesting
   the cookie programmatically was evaluated in the study and left out:
   Google may block embedded-webview sign-in, and the manual paste flow is
   upstream's proven UX. The UI must never ask for a Google password.
6. **Hand-rolled protobuf.** The contracts are blackbox-derived; several
   fields are fingerprint theatre (empty nested messages, the opaque
   result-item mask, `CommitUpload`'s deep empty tree). A generated-codec
   port risks normalising exactly the bytes the server fingerprints, so
   the encoder is a small explicit wire writer, verified byte-for-byte
   against Go (see below).
7. **Error distinctions are preserved**, because the UI depends on them:
   `BadAuthentication` vs `NeedsBrowser` (auth), album-not-found (404),
   and commit-ambiguous (never retry).

## Byte-fixture discipline (how protocol changes are kept safe)

`crates/outh-core/tests/protocol_fixtures.rs` pins the exact request bytes
for GetUploadToken, HashCheck, CommitUpload (default + Saver), CreateAlbum
and AddMediaToAlbum, plus decode fixtures for RemoteMatches /
CreateMediaItemsResponse and the Scotty token validation matrix from
`core/scotty_token.go`. Every fixture is derived twice:

- hand-computed from the `.proto` field numbers + api.go construction, and
- generated by `tools/gen-fixtures`, a Go harness that marshals the same
  messages with gotohp's own generated types into
  `crates/outh-core/tests/fixtures/golden.txt`.

Any protocol edit must keep both derivations in agreement; if they
diverge, the Go bytes are authoritative.

## Protocol drift and re-capture (maintenance)

This is study risk **R-3**, the most likely long-term failure mode: every
endpoint is a private numeric RPC id, the schemas were reverse-engineered
from captured traffic (`.proto/readme.md` in the gotohp repo describes the
blackboxprotobuf workflow), and the auth exchange leans on undocumented
behaviour (including a dummy DroidGuard value). Google can change any of
it, server-side, silently, per account or region. Expected symptoms:
uploads start failing (often at one stage only), new accounts cannot be
added while existing master tokens keep working, or commits return 2xx
with unparsable bodies.

When that happens:

1. **Check upstream first.** xob0t/gotohp is the cheapest drift insurance —
   diff its `core/` and `.proto/` against the commit Outh was ported from
   (`97a5dc0`). Most fixes will be constants: endpoint ids, magic
   `x-goog-ext-*` headers, device fingerprint, or a schema field.
2. **Keep all protocol constants in one place** (endpoint ids, headers,
   device identity, quality/policy mapping) so a drift fix is a
   one-file change plus fixture updates, mirroring where they live in
   `core/api.go` upstream.
3. **Re-capture if upstream has not fixed it.** That needs a real Android
   device with the official Photos app, traffic capture (the upstream
   `.proto/readme.md` workflow), and a throwaway Google account — update
   the field numbers, regenerate `golden.txt` with the updated gotohp
   clone (`tools/gen-fixtures`), and adjust the hand derivations in the
   Rust tests to match the new captures.
4. **Never test drift fixes against a primary account.** Development and
   QA use a throwaway account: the app impersonates a Pixel client, and
   account enforcement is a real (if unquantifiable) risk, as the README's
   honest-labelling note states.
