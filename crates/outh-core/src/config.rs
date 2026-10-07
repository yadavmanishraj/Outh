//! Config service — port of gotohp `core/configmanager.go`.
//!
//! Go kept a global `AppConfig` behind a mutex, persisted as YAML
//! (`gotohp.config`). Outh keeps the same *shape* (accounts + preferences)
//! in an instance-based [`ConfigService`] persisted as JSON
//! (CONTRACT.md decision 5):
//!
//! - Location: a portable `outh.config.json` next to the executable (then
//!   the working directory) wins if present — mirroring Go's
//!   `determineConfigPath` lookup order — otherwise
//!   `%APPDATA%\Outh\config.json`.
//! - Writes are atomic (temp file + rename in the same directory), like
//!   Go's `writeConfigAtomically`.
//! - Credentials pass through a [`CredentialProtector`] before touching
//!   disk: the stored JSON only ever holds *protected* credential strings.
//!   In memory, [`Config`] holds plaintext (unprotected) credentials.
//! - On first run, a legacy Go config (`gotohp.config`, sectioned or flat
//!   YAML) is imported and rewritten as JSON — porting Go's
//!   `migrateLegacyConfig` / `isLegacyConfig` behaviour via a minimal
//!   hand-written parser for that two-section YAML shape (no YAML crate is
//!   in the workspace dependency list).
//!
//! Deliberate simplifications/deviations from Go, flagged for review:
//! - Go's `AlbumName`/`AlbumAutoMode` are session-only preferences; they
//!   live on [`Preferences`] but are `#[serde(skip)]`, never persisted.
//! - Go's setters swallow save errors; Outh propagates them.
//! - Go's ADB token-binding extraction is not ported (v1, CONTRACT.md).

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::credential::Credential;
use crate::{Error, Result};

// ---------------------------------------------------------------------------
// Preferences (Preferences in configmanager.go)
// ---------------------------------------------------------------------------

/// User preferences. Defaults **must** match Go's `DefaultPreferences`:
///
/// | Field | Go default |
/// |---|---|
/// | `proxy` | `""` |
/// | `use_quota` | `false` |
/// | `saver` | `false` |
/// | `recursive` | `false` |
/// | `force_upload` | `false` |
/// | `pair_live_photos` | `false` |
/// | `skip_incomplete_live_photos` | **`true`** |
/// | `update_existing_to_live` | `false` |
/// | `delete_from_host` | `false` |
/// | `disable_unsupported_filter` | `false` |
/// | `set_date_from_filename` | `false` |
/// | `exclude_pattern` | `""` |
/// | `upload_threads` | **`3`** |
/// | `album_name` / `album_auto_mode` | session-only, never persisted |
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub proxy: String,
    #[serde(rename = "useQuota")]
    pub use_quota: bool,
    pub saver: bool,
    pub recursive: bool,
    #[serde(rename = "forceUpload")]
    pub force_upload: bool,
    #[serde(rename = "pairLivePhotos")]
    pub pair_live_photos: bool,
    #[serde(rename = "skipIncompleteLivePhotos")]
    pub skip_incomplete_live_photos: bool,
    /// Go field name: `UpdateExistingPhotosToLive`.
    #[serde(rename = "updateExistingPhotosToLive")]
    pub update_existing_to_live: bool,
    #[serde(rename = "deleteFromHost")]
    pub delete_from_host: bool,
    /// Go field name: `DisableUnsupportedFilesFilter`.
    #[serde(rename = "disableUnsupportedFilesFilter")]
    pub disable_unsupported_filter: bool,
    #[serde(rename = "setDateFromFilename")]
    pub set_date_from_filename: bool,
    #[serde(rename = "excludePattern")]
    pub exclude_pattern: String,
    #[serde(rename = "uploadThreads")]
    pub upload_threads: u32,
    /// App theme: "system" | "light" | "dark" (app-level addition — Go
    /// has no equivalent). Persisted, unlike the album fields below;
    /// `#[serde(default)]` keeps pre-theme config files loading (empty
    /// string = system).
    #[serde(default)]
    pub theme: String,
    /// Session-only (Go: `koanf:"-"`), never written to disk.
    #[serde(skip)]
    pub album_name: String,
    /// Session-only (Go: `koanf:"-"`), never written to disk.
    #[serde(skip)]
    pub album_auto_mode: bool,
}

impl Default for Preferences {
    /// Go `DefaultPreferences = Preferences{SkipIncompleteLivePhotos: true,
    /// UploadThreads: 3}` — every other field takes the zero value.
    fn default() -> Self {
        Preferences {
            proxy: String::new(),
            use_quota: false,
            saver: false,
            recursive: false,
            force_upload: false,
            pair_live_photos: false,
            skip_incomplete_live_photos: true,
            update_existing_to_live: false,
            delete_from_host: false,
            disable_unsupported_filter: false,
            set_date_from_filename: false,
            exclude_pattern: String::new(),
            upload_threads: 3,
            theme: String::new(),
            album_name: String::new(),
            album_auto_mode: false,
        }
    }
}

/// Per-run API policy — the subset of Go's `ApiOptions` (api.go) that
/// preferences can supply: `{ Proxy, Saver, UseQuota }` (`Account` is chosen
/// from [`Config::active_email`] instead of travelling inside the options).
/// Defined here, next to its source data; `api.rs` consumes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApiOptions {
    pub proxy: String,
    pub saver: bool,
    pub use_quota: bool,
}

impl Preferences {
    /// Port of Go `Preferences.UploadOptions()`'s Api half:
    /// `ApiOptions{Proxy: c.Proxy, Saver: c.Saver, UseQuota: c.UseQuota}`.
    pub fn api_options(&self) -> ApiOptions {
        ApiOptions {
            proxy: self.proxy.clone(),
            saver: self.saver,
            use_quota: self.use_quota,
        }
    }

    /// Full port of Go `Preferences.UploadOptions()` (upload_options.go),
    /// deriving the run options owned by `crate::upload::UploadOptions`.
    /// `IgnoreAppleMetadata` is never persisted and defaults to `false`,
    /// as in Go.
    pub fn upload_options(&self) -> crate::upload::UploadOptions {
        crate::upload::UploadOptions {
            api: self.api_options(),
            recursive: self.recursive,
            exclude_pattern: self.exclude_pattern.clone(),
            threads: self.upload_threads,
            force_upload: self.force_upload,
            delete_from_host: self.delete_from_host,
            disable_unsupported_filter: self.disable_unsupported_filter,
            set_date_from_filename: self.set_date_from_filename,
            pair_live_photos: self.pair_live_photos,
            skip_incomplete_live_photos: self.skip_incomplete_live_photos,
            update_existing_to_live: self.update_existing_to_live,
            ignore_apple_metadata: false,
            album_name: self.album_name.clone(),
            album_auto_mode: self.album_auto_mode,
        }
    }
}

// ---------------------------------------------------------------------------
// Config (Config / AccountConfig in configmanager.go)
// ---------------------------------------------------------------------------

/// One stored account. `credential` is the plaintext credential string in
/// memory and the **protected** string on disk — [`ConfigService`] converts
/// at the load/save boundary via its [`CredentialProtector`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub email: String,
    pub credential: String,
    pub needs_token_binding: bool,
}

impl Account {
    /// Build an account from a plaintext credential string, deriving the
    /// email and token-binding flag from the credential itself.
    pub fn from_credential(credential: &str) -> Result<Account> {
        let parsed = Credential::parse(credential)?;
        Ok(Account {
            email: parsed.email().to_string(),
            credential: credential.to_string(),
            needs_token_binding: parsed.needs_token_binding(),
        })
    }
}

/// Port of Go `AccountSummary` (configmanager.go).
#[derive(Clone, Debug, PartialEq)]
pub struct AccountSummary {
    pub email: String,
    pub needs_token_binding: bool,
}

/// The persisted configuration (Go `Config`): accounts + preferences.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub accounts: Vec<Account>,
    /// Go `AccountConfig.Selected`; `None` when no account is selected.
    #[serde(rename = "activeEmail")]
    pub active_email: Option<String>,
    pub preferences: Preferences,
}

impl Default for Config {
    /// Go `DefaultConfig = Config{Preferences: DefaultPreferences}`.
    fn default() -> Self {
        Config {
            accounts: Vec::new(),
            active_email: None,
            preferences: Preferences::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Credential protection
// ---------------------------------------------------------------------------

/// Protects credential strings at rest. The app supplies a DPAPI-backed
/// implementation on Windows; tests use [`IdentityProtector`].
pub trait CredentialProtector: Send + Sync {
    /// Plaintext credential string → stored representation.
    fn protect(&self, plain: &str) -> Result<String>;
    /// Stored representation → plaintext credential string.
    fn unprotect(&self, protected: &str) -> Result<String>;
}

/// Pass-through protector (tests, and the v1 placeholder until the app
/// wires DPAPI — see CONTRACT.md decision 5).
pub struct IdentityProtector;

impl CredentialProtector for IdentityProtector {
    fn protect(&self, plain: &str) -> Result<String> {
        Ok(plain.to_string())
    }
    fn unprotect(&self, protected: &str) -> Result<String> {
        Ok(protected.to_string())
    }
}

// ---------------------------------------------------------------------------
// ConfigService
// ---------------------------------------------------------------------------

/// Loads, mutates and persists [`Config`]. All mutations save immediately,
/// mirroring Go's `updateAppConfig`. Wrap in a `Mutex` if shared.
pub struct ConfigService {
    path: PathBuf,
    config: Config,
    protector: Arc<dyn CredentialProtector>,
}

impl ConfigService {
    /// Load from the resolved default path ([`config_path`]). If that file
    /// does not exist yet, a legacy Go `gotohp.config` found in Go's lookup
    /// locations is imported once and rewritten as JSON.
    pub fn load_default(protector: Arc<dyn CredentialProtector>) -> Result<ConfigService> {
        let path = config_path();
        if !path.exists() {
            for candidate in legacy_config_candidates() {
                if candidate.exists() {
                    if let Ok(mut service) =
                        ConfigService::load_from(&candidate, protector.clone())
                    {
                        // Re-home the imported config at the Outh path.
                        service.path = path.clone();
                        service.save()?;
                        return Ok(service);
                    }
                }
            }
        }
        ConfigService::load_from(&path, protector)
    }

    /// Load from an explicit path. Missing/empty file → defaults
    /// (Go `loadConfigLocked`). A file that is not JSON is treated as a
    /// legacy Go YAML config: migrated in memory and rewritten at the same
    /// path as JSON, so migration happens once (Go `migrateLegacyConfig`).
    /// An unparseable JSON file falls back to defaults, as Go's loader does
    /// on unmarshal errors.
    pub fn load_from(path: &std::path::Path, protector: Arc<dyn CredentialProtector>) -> Result<ConfigService> {
        let mut service = ConfigService {
            path: path.to_path_buf(),
            config: Config::default(),
            protector,
        };
        let Ok(bytes) = std::fs::read(path) else {
            return Ok(service); // missing file: defaults, like Go
        };
        if bytes.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(service); // empty file: defaults, like Go
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if text.trim_start().starts_with('{') {
            match serde_json::from_str::<Config>(&text) {
                Ok(mut stored) => {
                    // On-disk credentials are protected; unprotect them.
                    for account in &mut stored.accounts {
                        account.credential = service.protector.unprotect(&account.credential)?;
                    }
                    normalize_preferences(&mut stored.preferences);
                    service.config = stored;
                }
                Err(_) => {
                    // Go logs and falls back to DefaultConfig.
                    service.config = Config::default();
                }
            }
            Ok(service)
        } else {
            // Legacy Go YAML (either layout): import + rewrite as JSON.
            service.config = parse_yaml_config(&text);
            service.save()?;
            Ok(service)
        }
    }

    /// Persist the in-memory config (credentials protected) atomically.
    pub fn save(&self) -> Result<()> {
        let mut stored = self.config.clone();
        for account in &mut stored.accounts {
            account.credential = self.protector.protect(&account.credential)?;
        }
        // Session-only album preferences ride along in memory but are
        // serde-skipped, exactly like Go's koanf:"-" fields.
        let json = serde_json::to_string_pretty(&stored)
            .map_err(|e| Error::Config(format!("serialize config: {e}")))?;
        write_atomically(&self.path, json.as_bytes())
    }

    /// The file this service persists to.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Snapshot of the whole config (in-memory, plaintext credentials).
    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn preferences(&self) -> &Preferences {
        &self.config.preferences
    }

    /// Replace preferences and persist (Go's per-field setters all funnel
    /// into one save; the app can mutate via [`ConfigService::update_preferences`]).
    pub fn set_preferences(&mut self, preferences: Preferences) -> Result<()> {
        self.config.preferences = preferences;
        self.save()
    }

    /// Mutate preferences in place and persist.
    pub fn update_preferences<F: FnOnce(&mut Preferences)>(&mut self, update: F) -> Result<()> {
        update(&mut self.config.preferences);
        self.save()
    }

    /// Account list for the UI — port of Go `GetAccounts`: malformed
    /// credentials and empty emails are skipped.
    pub fn accounts(&self) -> Vec<AccountSummary> {
        self.config
            .accounts
            .iter()
            .filter(|a| !a.email.is_empty())
            .map(|a| AccountSummary {
                email: a.email.clone(),
                needs_token_binding: a.needs_token_binding,
            })
            .collect()
    }

    /// The active (selected) account, if any — Go `AccountConfig.Selected`.
    pub fn active_account(&self) -> Option<&Account> {
        let active = self.config.active_email.as_deref()?;
        self.config
            .accounts
            .iter()
            .find(|a| a.email.eq_ignore_ascii_case(active))
    }

    /// Insert or replace a credential and select it — port of
    /// `upsertCredential` (googleauth.go): same-email (case-insensitive)
    /// entries are replaced; on save failure the in-memory change is
    /// rolled back, as in Go. Returns the account email.
    pub fn upsert_credential(&mut self, credential: &str) -> Result<String> {
        let parsed = Credential::parse(credential)
            .map_err(|e| Error::Config(format!("parse generated credential: {e}")))?;
        let email = parsed.email().to_string();
        let previous_accounts = self.config.accounts.clone();
        let previous_active = self.config.active_email.clone();

        let account = Account {
            email: email.clone(),
            credential: credential.to_string(),
            needs_token_binding: parsed.needs_token_binding(),
        };
        if let Some(existing) = self
            .config
            .accounts
            .iter_mut()
            .find(|a| a.email.eq_ignore_ascii_case(&email))
        {
            *existing = account;
        } else {
            self.config.accounts.push(account);
        }
        self.config.active_email = Some(email.clone());

        if let Err(e) = self.save() {
            self.config.accounts = previous_accounts;
            self.config.active_email = previous_active;
            return Err(e);
        }
        Ok(email)
    }

    /// Import a raw credential string — port of `AddCredentials`
    /// (configmanager.go): required-field validation, duplicate email
    /// rejected (exact match, as in Go), appended and selected.
    /// (Deviation: Go ignores the save error; Outh propagates it.)
    pub fn add_credentials(&mut self, raw: &str) -> Result<String> {
        let parsed = Credential::parse(raw)
            .map_err(|e| Error::Auth(format!("invalid auth string format: {e}")))?;
        parsed.validate_import()?;
        let email = parsed.email().to_string();
        if self.config.accounts.iter().any(|a| a.email == email) {
            return Err(Error::Auth(format!(
                "auth string with email {email} already exists"
            )));
        }
        self.config.accounts.push(Account {
            email: email.clone(),
            credential: raw.to_string(),
            needs_token_binding: parsed.needs_token_binding(),
        });
        self.config.active_email = Some(email.clone());
        self.save()?;
        Ok(email)
    }

    /// Remove an account — port of `RemoveCredentials` (configmanager.go):
    /// exact email match; removing the selected account clears the
    /// selection. (Deviation: Go ignores the save error; Outh propagates.)
    pub fn remove_account(&mut self, email: &str) -> Result<()> {
        if email.is_empty() {
            return Err(Error::Config("email cannot be empty".into()));
        }
        let before = self.config.accounts.len();
        self.config.accounts.retain(|a| a.email != email);
        if self.config.accounts.len() == before {
            return Err(Error::Config(format!(
                "no credentials found for email {email}"
            )));
        }
        if self.config.active_email.as_deref() == Some(email) {
            self.config.active_email = None;
        }
        self.save()
    }

    /// Select the active account — port of `SetSelected`.
    pub fn set_active(&mut self, email: &str) -> Result<()> {
        self.config.active_email = if email.is_empty() {
            None
        } else {
            Some(email.to_string())
        };
        self.save()
    }

    /// Whether a stored credential needs token binding — port of
    /// `CredentialNeedsTokenBinding` (configmanager.go).
    pub fn credential_needs_token_binding(credential: &str) -> bool {
        Credential::parse(credential)
            .map(|c| c.needs_token_binding())
            .unwrap_or(false)
    }
}

/// Go `loadAppConfig` post-load fixups shared by every load path: the
/// `skip_incomplete_live_photos` default is handled by serde defaults for
/// JSON, but `upload_threads < 1` is always repaired to the default 3.
fn normalize_preferences(prefs: &mut Preferences) {
    if prefs.upload_threads < 1 {
        prefs.upload_threads = Preferences::default().upload_threads;
    }
}

// ---------------------------------------------------------------------------
// Paths (determineConfigPath / existingLocalConfig in configmanager.go)
// ---------------------------------------------------------------------------

/// Resolve the config file location, mirroring Go's lookup order:
/// portable `outh.config.json` next to the executable, then in the working
/// directory, wins over `%APPDATA%\Outh\config.json`.
pub fn config_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("outh.config.json");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("outh.config.json");
        if candidate.exists() {
            return candidate;
        }
    }
    user_config_dir().join("Outh").join("config.json")
}

/// Locations probed for a legacy Go `gotohp.config` on first run (Go:
/// exe dir, working dir, then `%APPDATA%\gotohp\gotohp.config`).
pub fn legacy_config_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("gotohp.config"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join("gotohp.config"));
    }
    out.push(user_config_dir().join("gotohp").join("gotohp.config"));
    out
}

/// Go `os.UserConfigDir()` on Windows is `%APPDATA%`.
fn user_config_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        if !appdata.is_empty() {
            return PathBuf::from(appdata);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join("AppData").join("Roaming");
        }
    }
    PathBuf::from(".")
}

// ---------------------------------------------------------------------------
// Atomic write (writeConfigAtomically in configmanager.go)
// ---------------------------------------------------------------------------

/// Write `contents` to `path` atomically: temp file in the same directory,
/// flush + sync, rename over the target. Creates parent directories.
pub fn write_atomically(path: &std::path::Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".outh-config-{}-{}.tmp",
        std::process::id(),
        crate::http::next_random_u64()
    ));
    let result = (|| -> Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

// ---------------------------------------------------------------------------
// Minimal parser for the legacy Go YAML shape (loadAppConfig /
// isLegacyConfig / migrateLegacyConfig in configmanager.go)
// ---------------------------------------------------------------------------

/// Parse either Go YAML layout into a [`Config`]:
///
/// - **sectioned** (current Go): top-level `account:` (`credentials:` list,
///   `selected:`) and `preferences:` sections with snake_case keys;
/// - **flat legacy**: the same keys at the top level.
///
/// Only the scalar/list shapes Go's writer produces are supported — this is
/// an importer for a known file, not a YAML implementation. Absent
/// preference keys keep Go defaults (which encodes both Go's
/// `DefaultPreferences` seeding during migration and the sectioned
/// loader's missing-`skip_incomplete_live_photos` fixup); a migrated
/// `upload_threads < 1` is repaired to 3 like Go does.
pub fn parse_yaml_config(text: &str) -> Config {
    let mut credentials: Vec<String> = Vec::new();
    let mut selected = String::new();
    let mut prefs = Preferences::default();
    // What the next "- item" lines belong to; only `credentials:` is a list
    // in either layout.
    let mut in_credentials_list = false;

    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ") {
            if in_credentials_list {
                credentials.push(yaml_unquote(item.trim()));
            }
            continue;
        }
        if trimmed == "-" {
            continue;
        }
        let Some((key, rest)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = rest.trim();
        in_credentials_list = false;
        match key {
            "account" | "preferences" => {
                // Section header (empty value): following keys dispatch by
                // name below, which is unambiguous across both layouts.
            }
            "credentials" => {
                if value.is_empty() {
                    in_credentials_list = true;
                } else if value == "[]" {
                    credentials.clear();
                }
            }
            "selected" => selected = yaml_unquote(value),
            "proxy" => prefs.proxy = yaml_unquote(value),
            "use_quota" => prefs.use_quota = yaml_bool(value),
            "saver" => prefs.saver = yaml_bool(value),
            "recursive" => prefs.recursive = yaml_bool(value),
            "force_upload" => prefs.force_upload = yaml_bool(value),
            "pair_live_photos" => prefs.pair_live_photos = yaml_bool(value),
            "skip_incomplete_live_photos" => {
                prefs.skip_incomplete_live_photos = yaml_bool(value)
            }
            "update_existing_photos_to_live" => {
                prefs.update_existing_to_live = yaml_bool(value)
            }
            "upload_threads" => {
                prefs.upload_threads = value.parse::<u32>().unwrap_or(0)
            }
            "delete_from_host" => prefs.delete_from_host = yaml_bool(value),
            "disable_unsupported_files_filter" => {
                prefs.disable_unsupported_filter = yaml_bool(value)
            }
            "set_date_from_filename" => {
                prefs.set_date_from_filename = yaml_bool(value)
            }
            "exclude_pattern" => prefs.exclude_pattern = yaml_unquote(value),
            _ => {}
        }
    }

    normalize_preferences(&mut prefs);
    let accounts = credentials
        .iter()
        .map(|raw| {
            let parsed = Credential::parse(raw).ok();
            Account {
                email: parsed
                    .as_ref()
                    .map(|c| c.email().to_string())
                    .unwrap_or_default(),
                credential: raw.clone(),
                needs_token_binding: parsed
                    .as_ref()
                    .map(|c| c.needs_token_binding())
                    .unwrap_or(false),
            }
        })
        .collect();
    Config {
        accounts,
        active_email: if selected.is_empty() {
            None
        } else {
            Some(selected)
        },
        preferences: prefs,
    }
}

fn yaml_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "on" | "1"
    )
}

/// Strip YAML quoting if present: single quotes double up (`''` → `'`),
/// double quotes process the common backslash escapes.
fn yaml_unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return value[1..value.len() - 1].replace("''", "'");
    }
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        let inner = &value[1..value.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "outh-config-test-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn identity() -> Arc<dyn CredentialProtector> {
        Arc::new(IdentityProtector)
    }

    /// Marker protector for the at-rest test: base64, so stored JSON never
    /// contains the plaintext credential.
    struct Base64Protector;
    impl CredentialProtector for Base64Protector {
        fn protect(&self, plain: &str) -> Result<String> {
            use base64::Engine;
            Ok(base64::engine::general_purpose::STANDARD.encode(plain.as_bytes()))
        }
        fn unprotect(&self, protected: &str) -> Result<String> {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(protected.as_bytes())
                .map_err(|e| Error::Config(format!("unprotect credential: {e}")))?;
            String::from_utf8(bytes).map_err(|e| Error::Config(format!("unprotect: {e}")))
        }
    }

    // Go DefaultPreferences / DefaultConfig.
    #[test]
    fn defaults_match_go() {
        let prefs = Preferences::default();
        assert!(prefs.skip_incomplete_live_photos);
        assert_eq!(prefs.upload_threads, 3);
        assert!(!prefs.use_quota && !prefs.saver && !prefs.recursive);
        assert!(!prefs.force_upload && !prefs.pair_live_photos);
        assert!(!prefs.update_existing_to_live && !prefs.delete_from_host);
        assert!(!prefs.disable_unsupported_filter && !prefs.set_date_from_filename);
        assert_eq!(prefs.proxy, "");
        assert_eq!(prefs.exclude_pattern, "");
        let config = Config::default();
        assert!(config.accounts.is_empty());
        assert_eq!(config.active_email, None);
    }

    // Port of TestLoadConfigMissingFileUsesDefaults.
    #[test]
    fn missing_file_uses_defaults() {
        let path = temp_dir("missing").join("config.json");
        let service = ConfigService::load_from(&path, identity()).unwrap();
        assert_eq!(service.config(), &Config::default());
    }

    // Port of TestLoadConfigMigratesLegacyFlatLayout: flat YAML in, JSON
    // out at the same path, second load reads the migrated file unchanged.
    #[test]
    fn legacy_flat_yaml_migrates_and_rewrites_as_json() {
        let dir = temp_dir("legacy");
        let path = dir.join("gotohp.config");
        let legacy = [
            "credentials:",
            "  - Email=person%40example.com&Token=secret",
            "selected: person@example.com",
            "proxy: http://proxy.example",
            "upload_threads: 7",
            "recursive: true",
            "skip_incomplete_live_photos: false",
            "",
        ]
        .join("\n");
        std::fs::write(&path, legacy).unwrap();

        let service = ConfigService::load_from(&path, identity()).unwrap();
        let config = service.config();
        assert_eq!(config.active_email.as_deref(), Some("person@example.com"));
        assert_eq!(config.accounts.len(), 1);
        assert_eq!(config.accounts[0].email, "person@example.com");
        let prefs = &config.preferences;
        assert_eq!(prefs.proxy, "http://proxy.example");
        assert_eq!(prefs.upload_threads, 7);
        assert!(prefs.recursive);
        assert!(!prefs.skip_incomplete_live_photos);

        // The file was rewritten in the new format (JSON), not left legacy.
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.trim_start().starts_with('{'), "{saved}");
        assert!(saved.contains("\"uploadThreads\": 7"), "{saved}");
        assert!(!saved.starts_with("credentials:"), "{saved}");

        // A second load reads the migrated layout and is identical.
        let again = ConfigService::load_from(&path, identity()).unwrap();
        assert_eq!(again.config(), service.config());
    }

    // The sectioned YAML layout Go currently writes must also import.
    #[test]
    fn sectioned_yaml_imports() {
        let dir = temp_dir("sectioned");
        let path = dir.join("gotohp.config");
        let yaml = [
            "account:",
            "    credentials:",
            "        - Email=person%40example.com&Token=secret&assertion_jwt=x",
            "    selected: person@example.com",
            "preferences:",
            "    proxy: http://proxy.example",
            "    upload_threads: 5",
            "    saver: true",
            "",
        ]
        .join("\n");
        std::fs::write(&path, yaml).unwrap();

        let service = ConfigService::load_from(&path, identity()).unwrap();
        let config = service.config();
        assert_eq!(config.accounts.len(), 1);
        assert!(config.accounts[0].needs_token_binding);
        assert_eq!(config.preferences.upload_threads, 5);
        assert!(config.preferences.saver);
        // Absent keys keep defaults (skip defaults true, as in Go).
        assert!(config.preferences.skip_incomplete_live_photos);
    }

    #[test]
    fn json_roundtrip_protects_credentials_at_rest() {
        let dir = temp_dir("protect");
        let path = dir.join("config.json");
        let protector: Arc<dyn CredentialProtector> = Arc::new(Base64Protector);
        let mut service = ConfigService::load_from(&path, protector.clone()).unwrap();
        let cred = Credential::build_photos("a@x.com", "aas_et/secret-token", "0123456789abcdef");
        service.upsert_credential(&cred.to_string()).unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("secret-token"), "{saved}");
        assert!(!saved.contains("aas_et"), "{saved}");

        let reloaded = ConfigService::load_from(&path, protector).unwrap();
        let account = reloaded.active_account().unwrap();
        assert_eq!(account.email, "a@x.com");
        let parsed = Credential::parse(&account.credential).unwrap();
        assert_eq!(parsed.master_token(), "aas_et/secret-token");
    }

    #[test]
    fn session_album_preferences_are_never_persisted() {
        let dir = temp_dir("session");
        let path = dir.join("config.json");
        let mut service = ConfigService::load_from(&path, identity()).unwrap();
        service
            .update_preferences(|p| {
                p.album_name = "Holidays".to_string();
                p.album_auto_mode = true;
                p.proxy = "http://proxy.example".to_string();
            })
            .unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("albumName"), "{saved}");
        assert!(!saved.contains("album_name"), "{saved}");
        assert!(saved.contains("proxy.example"), "{saved}");
    }

    // Port of the AddCredentials / RemoveCredentials behaviours.
    #[test]
    fn add_credentials_validates_and_rejects_duplicates() {
        let dir = temp_dir("crud");
        let path = dir.join("config.json");
        let mut service = ConfigService::load_from(&path, identity()).unwrap();

        let full = Credential::build_photos("a@x.com", "tok", "0123456789abcdef");
        let raw = full.to_string();
        assert_eq!(service.add_credentials(&raw).unwrap(), "a@x.com");
        assert_eq!(service.config().active_email.as_deref(), Some("a@x.com"));

        // Duplicate email rejected (Go: "already exists").
        let err = service.add_credentials(&raw).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");

        // Missing required fields rejected.
        let err = service
            .add_credentials("Email=b%40x.com&Token=t")
            .unwrap_err();
        assert!(err.to_string().contains("missing required fields"), "{err}");

        // Upsert replaces instead of duplicating (Embedded Setup path).
        let newer = Credential::build_photos("a@x.com", "tok2", "fedcba9876543210");
        service.upsert_credential(&newer.to_string()).unwrap();
        assert_eq!(service.config().accounts.len(), 1);
        let parsed =
            Credential::parse(&service.config().accounts[0].credential).unwrap();
        assert_eq!(parsed.master_token(), "tok2");

        // Removal clears the selection.
        service.remove_account("a@x.com").unwrap();
        assert!(service.config().accounts.is_empty());
        assert_eq!(service.config().active_email, None);
        let err = service.remove_account("a@x.com").unwrap_err();
        assert!(err.to_string().contains("no credentials found"), "{err}");
    }

    // Port of TestWriteConfigAtomicallyReplacesExistingFile.
    #[test]
    fn write_atomically_replaces_existing_file() {
        let dir = temp_dir("atomic");
        let path = dir.join("config.json");
        std::fs::write(&path, b"old").unwrap();
        write_atomically(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn preferences_derive_api_and_upload_options_like_go() {
        let mut prefs = Preferences::default();
        prefs.proxy = "http://proxy.example".to_string();
        prefs.saver = true;
        prefs.use_quota = true;
        prefs.album_name = "Trips".to_string();

        let api = prefs.api_options();
        assert_eq!(api.proxy, "http://proxy.example");
        assert!(api.saver && api.use_quota);

        let options = prefs.upload_options();
        assert_eq!(options.api, api);
        assert_eq!(options.threads, 3);
        assert!(options.skip_incomplete_live_photos);
        assert_eq!(options.album_name, "Trips");
        assert!(!options.ignore_apple_metadata);
    }
}
