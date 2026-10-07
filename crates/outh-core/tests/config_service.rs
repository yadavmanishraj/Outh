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
//! ASSUMED API — CONTRACT.md names the pieces but not the methods
//! ("ConfigService { load/save, CRUD, active account; protector injected }",
//!  "the app provides a DPAPI implementation on Windows, core tests use the
//!   identity protector"). The names below are the most direct reading; the
//! config agent's final API wins and this file adapts mechanically:
//!   outh_core::config::{
//!       ConfigService, IdentityProtector,
//!   }
//!   outh_core::config::CredentialProtector  (trait, Send + Sync)
//!       fn protect(&self, plain: &str) -> Result<String, Error>
//!       fn unprotect(&self, stored: &str) -> Result<String, Error>
//!   ConfigService::load(path: &Path, protector: Arc<dyn CredentialProtector>)
//!       -> Result<ConfigService, Error>   (missing file = defaults)
//!   ConfigService::config(&self) -> &Config
//!   ConfigService::config_mut(&mut self) -> &mut Config
//!   ConfigService::save(&mut self) -> Result<(), Error>
//! If the protector trait lands in credential.rs instead of config.rs, only
//! the use-line changes.
//! ============================================================================

use std::path::PathBuf;
use std::sync::Arc;

use outh_core::config::{
    Account, Config, ConfigService, CredentialProtector, IdentityProtector, Preferences,
};
use outh_core::Error;

fn temp_config_path(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("outh-test-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir.join("config.json")
}

/// A test protector that visibly transforms the value, so the tests can
/// prove the service actually routes credentials through the protector.
struct PrefixProtector;

impl CredentialProtector for PrefixProtector {
    fn protect(&self, plain: &str) -> Result<String, Error> {
        Ok(format!("protected:{plain}"))
    }
    fn unprotect(&self, stored: &str) -> Result<String, Error> {
        stored
            .strip_prefix("protected:")
            .map(str::to_string)
            .ok_or_else(|| Error::Config("stored credential lacks prefix".to_string()))
    }
}

const RAW_CREDENTIAL: &str = "Email=person%40example.com&Token=aas_et%2Ftest-master-token";

fn config_with_account() -> Config {
    Config {
        accounts: vec![Account {
            email: "person@example.com".to_string(),
            credential: RAW_CREDENTIAL.to_string(),
            needs_token_binding: true,
        }],
        active_email: Some("person@example.com".to_string()),
        preferences: Preferences {
            upload_threads: 7,
            recursive: true,
            ..Preferences::default()
        },
    }
}

#[test]
fn missing_file_loads_defaults() {
    // Port of TestLoadConfigMissingFileUsesDefaults.
    let path = temp_config_path("missing");
    let _ = std::fs::remove_file(&path);
    let svc = ConfigService::load(&path, Arc::new(IdentityProtector)).expect("load missing file");
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

    let mut svc = ConfigService::load(&path, Arc::new(IdentityProtector)).expect("load");
    *svc.config_mut() = config_with_account();
    svc.save().expect("save");
    assert!(path.exists(), "config file must exist after save");

    let loaded = ConfigService::load(&path, Arc::new(IdentityProtector)).expect("reload");
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

    let mut svc = ConfigService::load(&path, Arc::new(PrefixProtector)).expect("load");
    *svc.config_mut() = config_with_account();
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

    let loaded = ConfigService::load(&path, Arc::new(PrefixProtector)).expect("reload");
    assert_eq!(loaded.config().accounts[0].credential, RAW_CREDENTIAL);
}

#[test]
fn save_replaces_existing_file_and_leaves_no_temp() {
    // Port of TestWriteConfigAtomicallyReplacesExistingFile: a second save
    // fully replaces the first, and the temp+rename dance leaves no litter.
    let path = temp_config_path("atomic");
    let _ = std::fs::remove_file(&path);

    let mut svc = ConfigService::load(&path, Arc::new(IdentityProtector)).expect("load");
    svc.save().expect("first save");
    let first = std::fs::read_to_string(&path).expect("read first");

    svc.config_mut().preferences.upload_threads = 9;
    svc.save().expect("second save");
    let second = std::fs::read_to_string(&path).expect("read second");
    assert_ne!(first, second);
    assert!(second.contains("\"upload_threads\":9") || second.contains("\"upload_threads\": 9"));

    let siblings: Vec<_> = std::fs::read_dir(path.parent().expect("parent"))
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != "config.json")
        .collect();
    assert!(siblings.is_empty(), "temp files left behind: {siblings:?}");

    let loaded = ConfigService::load(&path, Arc::new(IdentityProtector)).expect("reload");
    assert_eq!(loaded.config().preferences.upload_threads, 9);
}
