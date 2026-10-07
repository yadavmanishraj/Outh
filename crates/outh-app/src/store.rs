//! Thin wrapper over `outh_core::config::ConfigService` — the ONLY place in
//! the app that talks to the config service, so that any mismatch between
//! the app and the final core API is a one-file fix at integration time.
//!
//! =====================================================================
//! INTEGRATION ASSUMPTIONS (CONTRACT.md freezes the config *types* —
//! `Config { accounts, active_email, preferences }`, `Account`,
//! `Preferences` — but not the `ConfigService` method names). This file
//! assumes:
//!
//! ```rust
//! impl ConfigService {
//!     pub fn load(protector: Arc<dyn CredentialProtector>)
//!         -> Result<ConfigService, Error>;   // resolves the config path
//!                                           // itself (%APPDATA%\Outh\…,
//!                                           // portable file wins — Go order)
//!     pub fn config(&self) -> &Config;
//!     pub fn add_account(&mut self, account: Account) -> Result<(), Error>;
//!         // upsert by email, persists
//!     pub fn remove_account(&mut self, email: &str) -> Result<(), Error>;
//!         // persists
//!     pub fn set_active(&mut self, email: &str) -> Result<(), Error>;
//!         // persists
//!     pub fn set_preferences(&mut self, prefs: Preferences)
//!         -> Result<(), Error>;              // persists
//! }
//! ```
//! Every method is assumed to persist on write, mirroring Go's
//! `updateAppConfig` (configmanager.go: every setter saves immediately).
//! =====================================================================

use std::sync::Arc;

use outh_core::config::{Account, ConfigService, CredentialProtector, Preferences};
use outh_core::{Error, Result};

use crate::protector::IdentityProtector;

/// Loads the config service with the v1 identity protector (see
/// `protector.rs` for the DPAPI TODO). Returns `(None, Some(error))` when
/// the on-disk config cannot be loaded; the app then runs on defaults and
/// surfaces the error instead of crashing at startup.
pub fn load() -> (Option<ConfigService>, Option<String>) {
    // TODO(DPAPI): swap IdentityProtector for the DPAPI protector here.
    let protector: Arc<dyn CredentialProtector> = Arc::new(IdentityProtector);
    match ConfigService::load(protector) {
        Ok(service) => (Some(service), None),
        Err(error) => (None, Some(format!("Could not load config: {error}"))),
    }
}

pub fn add_account(service: &mut ConfigService, account: Account) -> Result<(), Error> {
    service.add_account(account)
}

pub fn remove_account(service: &mut ConfigService, email: &str) -> Result<(), Error> {
    service.remove_account(email)
}

pub fn set_active(service: &mut ConfigService, email: &str) -> Result<(), Error> {
    service.set_active(email)
}

pub fn save_preferences(service: &mut ConfigService, preferences: &Preferences) -> Result<(), Error> {
    service.set_preferences(preferences.clone())
}
