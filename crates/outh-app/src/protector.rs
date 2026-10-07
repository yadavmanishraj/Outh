//! Credential protection for stored accounts (CONTRACT.md decision 5).
//!
//! v1 ships an *identity* protector: credentials pass through unchanged.
//! This is exactly what gotohp does (plaintext YAML at rest) and is NOT the
//! end state — it exists so the app builds before the DPAPI work lands.
//!
//! TODO(DPAPI): replace `IdentityProtector` with a Windows DPAPI
//! implementation (`CryptProtectData` / `CryptUnprotectData`, CurrentUser
//! scope) behind this same `CredentialProtector` trait. Nothing else in the
//! app changes when that happens: the protector is injected into
//! `ConfigService` at startup (see `store.rs`). Do NOT invent interim
//! "encryption" (XOR, base64, home-grown ciphers) — identity + this TODO is
//! the honest v1, per the build contract.
//!
//! INTEGRATION ASSUMPTION: the trait is `outh_core::config::CredentialProtector`
//! with `protect`/`unprotect` methods taking and returning `String`. If core
//! places the trait elsewhere or names the methods differently, only this
//! file (and the `Arc<dyn CredentialProtector>` construction in `store.rs`)
//! needs to change.

use outh_core::config::CredentialProtector;
use outh_core::Result;

/// Pass-through protector: stores the credential string as-is.
pub struct IdentityProtector;

impl CredentialProtector for IdentityProtector {
    fn protect(&self, plaintext: &str) -> Result<String> {
        Ok(plaintext.to_string())
    }

    fn unprotect(&self, protected: &str) -> Result<String> {
        Ok(protected.to_string())
    }
}
