# Outh — build contract (frozen for the first build wave)

Outh is a Rust reimplementation of [xob0t/gotohp](https://github.com/xob0t/gotohp) (Google Photos
desktop uploader) on **windows-rs / windows-reactor** (WinUI 3). Ground truth:

- gotohp Go source: `~/workspace/gotohp-study/gotohp` (HEAD 97a5dc0) — port `core/*.go` faithfully,
  file by file. Field numbers in `.proto/*.proto` + `generated/*.pb.go` are the wire contract.
- Study docs: `~/workspace/sdlc/gotohp/` (esp. GOTOH_STUDY.md, RESEARCH_2/3/5),
  `~/workspace/sdlc/windows-rs/` (esp. RESEARCH_5/6 reactor, RESEARCH_8 playbook).
- Reactor API examples: `~/workspace/windows-rs-study/windows-rs/crates/samples/reactor/*` and
  `crates/libs/reactor/src/` — copy idioms from source, never invent reactor API.

## Decisions (frozen)

1. **Rust + windows-reactor**, workspace: `crates/outh-core` (platform-independent library) +
   `crates/outh-app` (reactor binary, self-contained via `windows-reactor-setup` build.rs).
2. **HTTP: `ureq` 3 + rustls**, synchronous. Counting-reader progress exactly like Go's
   `progress_reader.go`. Chunked PUT with unknown length (do NOT switch to known-length).
   Redirects: never auto-follow on auth endpoints. **TLS verification is NEVER disabled**
   (deliberate deviation from Go's proxy `InsecureSkipVerify`).
3. **Protobuf: hand-rolled wire format** (`wire.rs`) — no prost/protobuf crates. Messages are
   small structs with `encode(&self, &mut Vec<u8>)` / decode from `&[u8]`. Scotty finalize token
   stays opaque raw bytes (`ScottyToken`), validated at wire level like `scotty_token.go`.
4. **Auth v1: Embedded Setup exchange + raw credential import.** Token binding is NOT
   implemented in v1 (Embedded-Setup credentials never need it); credentials carry a
   `needs_token_binding` flag so the UI can badge imported credentials that require it.
   Never build UI that asks for a Google password.
5. **Config:** JSON (serde) at `%APPDATA%\Outh\config.json` (portable `outh.config.json` next to
   the exe wins if present, mirroring Go's lookup order where sensible). Atomic write
   (temp + rename). Credentials are stored via the `CredentialProtector` trait; the app provides
   a DPAPI implementation on Windows, core tests use the identity protector. Do not store
   credentials anywhere else; never log credential material.
6. **"Unlimited" semantics preserved** (Pixel XL + storagePolicy 3 default; Saver = Pixel 2/1;
   UseQuota forces Pixel 8) and labelled honestly in the UI.
7. Concurrency: core upload engine uses `std::thread` workers over a crossbeam-free
   `std::sync::mpsc` design + `AtomicBool`/shared `CancellationToken` (simple struct wrapping
   `Arc<AtomicBool>`). No tokio anywhere.
8. Commit messages: Conventional Commits (per gotohp's AGENTS.md style).

## Core public surface (outh-core) — implement exactly these

```rust
// types.rs
pub struct CancellationToken(/* Arc<AtomicBool> */); // new(), cancel(), is_cancelled()
pub enum Stage { Hashing, Checking, Uploading, Finalizing, Completed, Skipped, Error }
pub struct ThreadStatus { pub worker: usize, pub stage: Stage, pub file_path: PathBuf,
    pub bytes_uploaded: u64, pub bytes_total: u64, pub attempt: u32 }
pub struct FileResult { pub file_path: PathBuf, pub outcome: Outcome, pub media_key: Option<String>,
    pub message: Option<String> }   // Outcome: Uploaded | SkippedAlreadyPresent | SkippedUnsupported | Failed
pub struct PreflightWarning { pub file_path: PathBuf, pub message: String }
pub struct AlbumStatus { pub name: String, pub total: usize, pub done: usize }
pub trait UploadReporter: Send + Sync {   // port of reporter.go, method names snake_case
    fn upload_start(&self, total_files: usize);
    fn upload_stop(&self);
    fn total_bytes(&self, total: u64);
    fn total_bytes_delta(&self, delta: i64);
    fn warning(&self, w: PreflightWarning);
    fn thread_status(&self, s: ThreadStatus);
    fn file_result(&self, r: FileResult);
    fn album_progress(&self, s: AlbumStatus);
    fn album_complete(&self, s: AlbumStatus);
    fn album_error(&self, name: String, message: String);
}
pub struct NullReporter; // impl UploadReporter with no-ops

// config.rs — port of configmanager.go + Preferences
pub struct Preferences { pub proxy: String, pub use_quota: bool, pub saver: bool,
    pub recursive: bool, pub force_upload: bool, pub pair_live_photos: bool,
    pub skip_incomplete_live_photos: bool, pub update_existing_to_live: bool,
    pub delete_from_host: bool, pub disable_unsupported_filter: bool,
    pub set_date_from_filename: bool, pub exclude_pattern: String,
    pub upload_threads: u32, pub album_name: String, pub album_auto_mode: bool }
    // Default impl MUST match Go defaults (read configmanager.go).
pub struct Account { pub email: String, pub credential: String /* protected at rest */, pub needs_token_binding: bool }
pub struct Config { pub accounts: Vec<Account>, pub active_email: Option<String>, pub preferences: Preferences }
pub struct ConfigService { /* load/save, CRUD, active account; protector injected */ }

// credential.rs — the Android credential string format (key=value&... query-style, see googleauth.go/configmanager.go)
pub struct Credential { /* parsed fields: email, master_token (aas_et), android_id, ... */ }
impl Credential { pub fn parse(s: &str) -> Result<Credential, Error>; pub fn to_string(&self) -> String;
    pub fn needs_token_binding(&self) -> bool; }

// auth.rs — port of googleauth.go
pub struct EmbeddedSetupAuth { /* injectable endpoints + http client factory for tests */ }
impl EmbeddedSetupAuth {
    pub fn exchange_oauth_token(&self, oauth_token: &str) -> Result<Credential, Error>; // -> master token -> Photos credential, validated like Go
}

// api.rs — port of api.go; ApiOptions { proxy, saver, use_quota }
pub struct PhotosClient { /* credential, bearer cache w/ expiry, http */ }
impl PhotosClient {
    pub fn new(cred: Credential, opts: ApiOptions) -> Result<Self, Error>;
    pub fn find_remote_media_by_hash(&self, sha1: &[u8;20]) -> Result<Option<String>, Error>;
    pub fn get_upload_token(&self, sha1: &[u8;20], size: u64) -> Result<UploadSession, Error>; // session id from X-GUploader-UploadID header
    pub fn upload_file(&self, session: &UploadSession, path: &Path, progress: &dyn Fn(u64,u64), cancel: &CancellationToken) -> Result<ScottyToken, Error>; // PUT, retry x3 in upload layer or here — match Go placement
    pub fn commit_upload(&self, token: &ScottyToken, file: &FileMeta) -> Result<String /*media key*/, Error>;
    pub fn commit_live_photo(...) -> ...; pub fn reconcile_live_photo(...) -> ...; // per create_media_items.go/livephoto_upload.go
    pub fn create_album(&self, name: &str) -> Result<String /*album key*/, Error>;
    pub fn add_media_to_album(&self, album_key: &str, media_keys: &[String]) -> Result<(), Error>;
}
pub struct FileMeta { pub name: String, pub size: u64, pub sha1: [u8;20], pub taken_unix_secs: i64 /* mtime or filename date */ }

// upload.rs — port of upload.go + album.go orchestration
pub struct UploadOptions { /* mirrors Go UploadOptions incl. normalized() */ }
pub struct UploadManager { /* new(photos_factory, reporter: Arc<dyn UploadReporter>) */ }
impl UploadManager { pub fn run(&self, inputs: Vec<PathBuf>, opts: UploadOptions, cancel: CancellationToken) -> RunSummary; }

// filename.rs — port of filename_parser.go (4 regex patterns, table tests)
// sha1calc.rs — streaming SHA-1, 1MiB buffer
// livephoto.rs / livephoto_metadata.rs / create_media_items.rs — ports of the same-named Go files
// protocol.rs + wire.rs — all 10 .proto messages needed by api.rs
```

Error type: one `Error` enum in `error.rs` (thiserror-style hand-written Display/Error impls —
do NOT add thiserror dep; keep deps to the workspace list). Variants must preserve Go's
distinctions the UI relies on: auth errors (`BadAuthentication`, `NeedsBrowser`, token rejected),
album-not-found (404), commit-ambiguous (2xx + unparseable body = never retry).

## App (outh-app) — reactor

- Single component tree in `src/main.rs` (+ modules as needed). Root `App` component with
  sections: **Upload** (path entry + add files/folders via windows-pickers reactor sample
  pattern, album mode: none/named/auto, queue + per-worker status + aggregate progress +
  results, Start/Cancel), **Accounts** (list, add via oauth_token paste → Embedded Setup,
  advanced raw-credential import, select active, remove, token-binding badge),
  **Settings** (all Preferences fields, persist on change).
- Upload runs off the UI thread via reactor `spawn_background` / `ComponentSender` messages;
  reporter impl forwards core callbacks into component messages.
- `build.rs`: `windows_reactor_setup::as_self_contained()`.
- Follow the design language of the samples; use NavigationView if the navigation samples make
  it straightforward, otherwise a simple section switcher. It must compile against the real
  0.100 API — read the samples.

## Hard rules for every agent

- Port behaviour from the Go source open in front of you; cite Go file in a comment where a
  constant is magic (endpoint IDs, x-goog-ext headers, device fingerprints, result_item_mask).
- Never log/print credential material. Never disable TLS verification.
- Tests: port the Go tests for your area (config migration, upload options, googleauth parser,
  filename table tests) + protocol byte-fixture tests.
- Your branch must at least `cargo check -p outh-core` conceptually — there is no Rust toolchain
  in the sandbox, so be extra careful with syntax/types; the laptop build is the gate.
