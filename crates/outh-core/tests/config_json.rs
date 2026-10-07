//! Config JSON + defaults tests (no file I/O, no ConfigService assumptions).
//!
//! Sources: core/configmanager.go — DefaultPreferences is EXACTLY
//! { SkipIncompleteLivePhotos: true, UploadThreads: 3 } with every other
//! field at its zero value, and DefaultConfig carries those preferences
//! with no credentials and no selection. Legacy-layout cases come from
//! core/config_migration_test.go.
//!
//! ============================================================================
//! ASSUMED API — from CONTRACT.md (config.rs section), which fixes the
//! struct shapes used here field-for-field:
//!   Preferences { proxy, use_quota, saver, recursive, force_upload,
//!     pair_live_photos, skip_incomplete_live_photos,
//!     update_existing_to_live, delete_from_host,
//!     disable_unsupported_filter, set_date_from_filename, exclude_pattern,
//!     upload_threads, album_name, album_auto_mode }
//!   Account { email, credential, needs_token_binding }
//!   Config { accounts, active_email, preferences }
//! Further assumptions:
//!   [B1] Preferences implements Default matching Go (CONTRACT.md states
//!        this outright: "Default impl MUST match Go defaults").
//!   [B2] Config/Preferences/Account derive serde Serialize+Deserialize
//!        (CONTRACT.md: "Config: JSON (serde)"). As landed in config.rs,
//!        the JSON keys are camelCase — Preferences carries per-field
//!        serde renames (useQuota, uploadThreads, ...) and Account uses
//!        rename_all = "camelCase" — while album_name/album_auto_mode
//!        are #[serde(skip)] (Go's koanf:"-"), so no assertion is made
//!        about those two keys.
//! ============================================================================

use outh_core::config::{Account, Config, Preferences};

#[test]
fn preferences_default_matches_go_defaults() {
    let p = Preferences::default();
    // The only two non-zero Go defaults (configmanager.go DefaultPreferences).
    assert!(p.skip_incomplete_live_photos);
    assert_eq!(p.upload_threads, 3);
    // Everything else is the zero value in Go.
    assert_eq!(p.proxy, "");
    assert!(!p.use_quota);
    assert!(!p.saver);
    assert!(!p.recursive);
    assert!(!p.force_upload);
    assert!(!p.pair_live_photos);
    assert!(!p.update_existing_to_live);
    assert!(!p.delete_from_host);
    assert!(!p.disable_unsupported_filter);
    assert!(!p.set_date_from_filename);
    assert_eq!(p.exclude_pattern, "");
    assert_eq!(p.album_name, "");
    assert!(!p.album_auto_mode);
}

fn sample_config() -> Config {
    Config {
        accounts: vec![Account {
            email: "person@example.com".to_string(),
            credential: "Email=person%40example.com&Token=aas_et%2Ftest-master-token".to_string(),
            needs_token_binding: false,
        }],
        active_email: Some("person@example.com".to_string()),
        preferences: Preferences {
            proxy: "http://proxy.example:8080".to_string(),
            use_quota: true,
            saver: false,
            recursive: true,
            force_upload: false,
            pair_live_photos: true,
            skip_incomplete_live_photos: false,
            update_existing_to_live: true,
            delete_from_host: false,
            disable_unsupported_filter: false,
            set_date_from_filename: true,
            exclude_pattern: "*.tmp".to_string(),
            upload_threads: 7,
            album_name: String::new(),
            album_auto_mode: false,
        },
    }
}

#[test]
fn config_json_roundtrip_preserves_everything() {
    let config = sample_config();
    let json = serde_json::to_string(&config).expect("serialize config");
    let back: Config = serde_json::from_str(&json).expect("deserialize config");

    assert_eq!(back.accounts.len(), 1);
    assert_eq!(back.accounts[0].email, "person@example.com");
    assert_eq!(back.accounts[0].credential, config.accounts[0].credential);
    assert!(!back.accounts[0].needs_token_binding);
    assert_eq!(back.active_email.as_deref(), Some("person@example.com"));

    let p = &back.preferences;
    assert_eq!(p.proxy, "http://proxy.example:8080");
    assert!(p.use_quota);
    assert!(!p.saver);
    assert!(p.recursive);
    assert!(!p.force_upload);
    assert!(p.pair_live_photos);
    assert!(!p.skip_incomplete_live_photos);
    assert!(p.update_existing_to_live);
    assert!(!p.delete_from_host);
    assert!(!p.disable_unsupported_filter);
    assert!(p.set_date_from_filename);
    assert_eq!(p.exclude_pattern, "*.tmp");
    assert_eq!(p.upload_threads, 7);
}

#[test]
fn config_json_shape_uses_camel_case_keys() {
    // [B2] The on-disk JSON is user-inspectable (Go's file was too); pin the
    // key spellings so a silent rename is a deliberate, reviewed change.
    // The spellings are the ones config.rs's serde renames define.
    let json = serde_json::to_string(&sample_config()).expect("serialize");
    for key in [
        "\"accounts\"",
        "\"activeEmail\"",
        "\"preferences\"",
        "\"uploadThreads\":7",
        "\"skipIncompleteLivePhotos\":false",
        "\"useQuota\":true",
        "\"excludePattern\":\"*.tmp\"",
        "\"needsTokenBinding\":false",
    ] {
        assert!(json.contains(key), "config JSON missing {key}: {json}");
    }
}

#[test]
fn empty_config_json_roundtrip() {
    let config = Config {
        accounts: Vec::new(),
        active_email: None,
        preferences: Preferences::default(),
    };
    let json = serde_json::to_string(&config).expect("serialize");
    let back: Config = serde_json::from_str(&json).expect("deserialize");
    assert!(back.accounts.is_empty());
    assert_eq!(back.active_email, None);
    assert_eq!(back.preferences.upload_threads, 3);
    assert!(back.preferences.skip_incomplete_live_photos);
}

#[test]
fn preferences_missing_keys_fall_back_to_go_defaults() {
    // A config written by an older build (or hand-edited) may omit keys.
    // [B3] Assumes Preferences' serde defaults are the Go defaults (e.g.
    // via #[serde(default)] + the Default impl), so an empty object decodes
    // to DefaultPreferences rather than all-zero values — note the trap:
    // an all-zero decode would flip skip_incomplete_live_photos to false.
    let p: Preferences = serde_json::from_str("{}").expect("deserialize empty preferences");
    assert!(p.skip_incomplete_live_photos);
    assert_eq!(p.upload_threads, 3);

    let p: Preferences =
        serde_json::from_str("{\"uploadThreads\":7,\"recursive\":true}").expect("partial");
    assert_eq!(p.upload_threads, 7);
    assert!(p.recursive);
    assert!(p.skip_incomplete_live_photos);
}
