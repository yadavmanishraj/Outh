//! ConfigService file roundtrip tests: JSON on disk, atomic save,
//! credential protection at rest.
//!
//! Behavioural sources: core/configmanager.go + core/googleauth_test.go
//! (TestWriteConfigAtomicallyReplacesExistingFile, missing-file defaults in
//! core/config_migration_test.go) and CONTRACT.md §5: config lives at a JSON
//! path, writes are atomic (temp + rename), and credentials are stored via
//! the injected CredentialProtector — never in plaintext by the service
//! itself, and core tests use an identity protector.
//!
//! ============================================================================
//! API — as landed in `outh_core::config` (adapted at integration):
//!   ConfigService::load_from(path: &Path, protector: Arc<dyn CredentialProtector>)
//!       -> Result<ConfigService, Error>   (missing file = defaults)
//!   ConfigService::config(&self) -> &Config
//!   ConfigService::save(&self) -> Result<(), Error>
//!   ConfigService::upsert_credential(&mut self, credential: &str)
//!       -> Result<String, Error>   (insert/replace + select; saves)
//!   ConfigService::update_preferences(&mut self, f: impl FnOnce(&mut Preferences))
//!       -> Result<(), Error>       (mutate + save)
//! There is no `config_mut`: state changes go through the mutation
//! methods above, which persist immediately, mirroring Go's
//! `updateAppConfig`. The tests therefore seed state through
//! `upsert_credential` + `update_preferences` (see `seed`).
//! ============================================================================

use std::path::PathBuf;
use std::sync::Arc;

use outh_core::config::{ConfigService, CredentialProtector, IdentityProtector};
use outh_core::Error;

fn temp_config_path(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("outh-test-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir.join("config.json")
}

/// A test protector that visibly transforms the value, so the tests can
/// prove the service actually routes credentials through the protector.
/// The transform (prefix + reversed text) is reversible but does not
/// embed the plaintext verbatim — a plain `format!("protected:{plain}")`
/// would leave the raw credential as a substring of the stored file and
/// defeat the at-rest assertion below.
struct PrefixProtector;

impl CredentialProtector for PrefixProtector {
    fn protect(&self, plain: &str) -> Result<String, Error> {
        Ok(format!(
            "protected:{}",
            plain.chars().rev().collect::<String>()
        ))
    }
    fn unprotect(&self, stored: &str) -> Result<String, Error> {
        stored
            .strip_prefix("protected:")
            .map(|rest| rest.chars().rev().collect())
            .ok_or_else(|| Error::Config("stored credential lacks prefix".to_string()))
    }
}

/// Carries `assertion_jwt`, so `Credential::needs_token_binding` derives
/// `true` for this account (see the reload assertions below).
const RAW_CREDENTIAL: &str = "Email=person%40example.com&Token=aas_et%2Ftest-master-token&assertion_jwt=eyJhbGciOiJFUzI1NiJ9.payload.sig";

/// Install the fixture account + preferences through the service's real
/// mutation API: `upsert_credential` derives the email and the
/// token-binding flag from the credential and selects the account;
/// `update_preferences` sets the non-default preferences.
fn seed(svc: &mut ConfigService) {
    svc.upsert_credential(RAW_CREDENTIAL).expect("upsert credential");
    svc.update_preferences(|p| {
        p.upload_threads = 7;
        p.recursive = true;
    })
    .expect("update preferences");
}

#[test]
fn missing_file_loads_defaults() {
    // Port of TestLoadConfigMissingFileUsesDefaults.
    let path = temp_config_path("missing");
    let _ = std::fs::remove_file(&path);
    let svc = ConfigService::load_from(&path, Arc::new(IdentityProtector)).expect("load missing file");
    let config = svc.config();
    assert!(config.accounts.is_empty());
    assert_eq!(config.active_email, None);
    assert_eq!(config.preferences.upload_threads, 3);
    assert!(config.preferences.skip_incomplete_live_photos);
}

#[test]
fn save_then_load_roundtrips_through_disk() {
    let path = temp_config_path("roundtrip");
    let _ = std::fs::remove_file(&path);

    let mut svc = ConfigService::load_from(&path, Arc::new(IdentityProtector)).expect("load");
    seed(&mut svc);
    svc.save().expect("save");
    assert!(path.exists(), "config file must exist after save");

    let loaded = ConfigService::load_from(&path, Arc::new(IdentityProtector)).expect("reload");
    let config = loaded.config();
    assert_eq!(config.accounts.len(), 1);
    assert_eq!(config.accounts[0].email, "person@example.com");
    assert_eq!(config.accounts[0].credential, RAW_CREDENTIAL);
    assert!(config.accounts[0].needs_token_binding);
    assert_eq!(config.active_email.as_deref(), Some("person@example.com"));
    assert_eq!(config.preferences.upload_threads, 7);
    assert!(config.preferences.recursive);
}

#[test]
fn credentials_are_protected_at_rest() {
    // CONTRACT.md §5: the service must not write the raw credential when a
    // real protector is injected — the file holds the protected form, and
    // loading unprotects transparently.
    let path = temp_config_path("protected");
    let _ = std::fs::remove_file(&path);

    let mut svc = ConfigService::load_from(&path, Arc::new(PrefixProtector)).expect("load");
    seed(&mut svc);
    svc.save().expect("save");

    let on_disk = std::fs::read_to_string(&path).expect("read saved config");
    assert!(
        !on_disk.contains(RAW_CREDENTIAL),
        "raw credential must not appear in the config file"
    );
    assert!(
        on_disk.contains("protected:"),
        "protected credential form expected in the config file"
    );

    let loaded = ConfigService::load_from(&path, Arc::new(PrefixProtector)).expect("reload");
    assert_eq!(loaded.config().accounts[0].credential, RAW_CREDENTIAL);
}

#[test]
fn save_replaces_existing_file_and_leaves_no_temp() {
    // Port of TestWriteConfigAtomicallyReplacesExistingFile: a second save
    // fully replaces the first, and the temp+rename dance leaves no litter.
    let path = temp_config_path("atomic");
    let _ = std::fs::remove_file(&path);

    let mut svc = ConfigService::load_from(&path, Arc::new(IdentityProtector)).expect("load");
    svc.save().expect("first save");
    let first = std::fs::read_to_string(&path).expect("read first");

    svc.update_preferences(|p| p.upload_threads = 9)
        .expect("update preferences");
    svc.save().expect("second save");
    let second = std::fs::read_to_string(&path).expect("read second");
    assert_ne!(first, second);
    assert!(second.contains("\"uploadThreads\":9") || second.contains("\"uploadThreads\": 9"));

    let siblings: Vec<_> = std::fs::read_dir(path.parent().expect("parent"))
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != "config.json")
        .collect();
    assert!(siblings.is_empty(), "temp files left behind: {siblings:?}");

    let loaded = ConfigService::load_from(&path, Arc::new(IdentityProtector)).expect("reload");
    assert_eq!(loaded.config().preferences.upload_threads, 9);
}
