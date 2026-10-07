//! Legacy gotohp YAML config import tests — FEATURE-GATED, INERT BY DEFAULT.
//!
//! Why gated: the frozen CONTRACT.md specifies JSON config only and defines
//! NO legacy-YAML import API. gotohp's own config, however, was YAML
//! (`gotohp.config`, with a pre-sectioned "flat" legacy layout that Go
//! migrates on load — core/configmanager.go legacyConfig + TestLoadConfig-
//! MigratesLegacyFlatLayout in core/config_migration_test.go), and users
//! coming from gotohp may want their accounts carried over.
//!
//! This file compiles to nothing unless the `legacy_yaml_import` feature is
//! added to outh-core AND the assumed API below is implemented:
//!   ConfigService::load detects a legacy YAML file at the config path and
//!   migrates it (same constructor as config_service.rs tests).
//! Until then it documents the exact migration cases the implementation
//! must satisfy, using Go's own test data verbatim.
#![cfg(feature = "legacy_yaml_import")]

use std::path::PathBuf;
use std::sync::Arc;

use outh_core::config::{ConfigService, IdentityProtector};

fn temp_legacy_path(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("outh-legacy-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir.join("gotohp.config")
}

#[test]
fn legacy_flat_yaml_migrates_to_sectioned_config() {
    // Verbatim port of TestLoadConfigMigratesLegacyFlatLayout (Go).
    let path = temp_legacy_path("flat");
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
    std::fs::write(&path, legacy).expect("write legacy config");

    let svc = ConfigService::load(&path, Arc::new(IdentityProtector)).expect("load legacy");
    let config = svc.config();
    assert_eq!(config.active_email.as_deref(), Some("person@example.com"));
    assert_eq!(config.accounts.len(), 1);
    assert_eq!(config.accounts[0].email, "person@example.com");
    assert_eq!(
        config.accounts[0].credential,
        "Email=person%40example.com&Token=secret"
    );
    let p = &config.preferences;
    assert_eq!(p.proxy, "http://proxy.example");
    assert_eq!(p.upload_threads, 7);
    assert!(p.recursive);
    assert!(!p.skip_incomplete_live_photos);
}

#[test]
fn legacy_sectioned_yaml_loads() {
    // The current (sectioned) gotohp YAML layout: account:/preferences:
    // with snake_case koanf keys (core/configmanager.go koanf tags).
    let path = temp_legacy_path("sectioned");
    let yaml = [
        "account:",
        "  credentials:",
        "    - Email=person%40example.com&Token=secret",
        "  selected: person@example.com",
        "preferences:",
        "  proxy: \"\"",
        "  use_quota: false",
        "  saver: true",
        "  recursive: false",
        "  force_upload: false",
        "  pair_live_photos: false",
        "  skip_incomplete_live_photos: true",
        "  update_existing_photos_to_live: false",
        "  upload_threads: 5",
        "  delete_from_host: false",
        "  disable_unsupported_files_filter: false",
        "  set_date_from_filename: false",
        "  exclude_pattern: \"\"",
        "",
    ]
    .join("\n");
    std::fs::write(&path, yaml).expect("write sectioned config");

    let svc = ConfigService::load(&path, Arc::new(IdentityProtector)).expect("load sectioned");
    let config = svc.config();
    assert_eq!(config.accounts.len(), 1);
    assert!(config.preferences.saver);
    assert_eq!(config.preferences.upload_threads, 5);
    assert!(config.preferences.skip_incomplete_live_photos);
}
