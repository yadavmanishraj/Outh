//! Credential parse / format / token-binding-flag tests.
//!
//! The credential format is the Android auth-request string: `key=value`
//! pairs joined with `&`, URL-query-escaped (Go: `url.Values.Encode()` —
//! keys sorted byte-wise, space → `+`, everything outside
//! `[A-Za-z0-9-_.~]` percent-escaped). gotohp replays this string
//! near-verbatim to `android.googleapis.com/auth` (core/api.go
//! getAuthToken copies every parsed key), so parse + re-format must be
//! lossless, including keys Outh does not know about.
//!
//! Cases are derived from core/googleauth.go (buildGooglePhotosCredential),
//! core/configmanager.go (credentialNeedsTokenBinding, AddCredentials
//! required fields) and core/googleauth_test.go.
//!
//! ============================================================================
//! ASSUMED API — from CONTRACT.md (credential.rs section), which specifies:
//!   pub struct Credential { /* parsed fields */ }
//!   Credential::parse(&str) -> Result<Credential, Error>
//!   Credential::to_string(&self) -> String     (via inherent method or
//!                                               Display/ToString — tests use
//!                                               `.to_string()` either way)
//!   Credential::needs_token_binding(&self) -> bool
//! Further assumptions flagged inline where they go beyond the contract:
//!   [A1] re-formatting reproduces Go's canonical `url.Values.Encode()`
//!        output exactly (sorted keys, Go escaping). The canonical fixture
//!        below was verified against real Go: `url.Values.Encode()` of the
//!        same map produces a 542-char string; the fixture is 542 chars and
//!        built by the same sorting/escaping rules.
//!   [A2] unknown keys survive a parse/format roundtrip (required by the
//!        near-verbatim replay in core/api.go, so a faithful port must).
//!   [A3] parse rejects syntactically invalid query escapes, as Go's
//!        url.ParseQuery does.
//! ============================================================================

use outh_core::credential::Credential;

/// The exact output of Go's buildGooglePhotosCredential(
///   "person@example.com", "aas_et/test-master-token", "0123456789abcdef")
/// (core/googleauth.go: values.Encode() of the fixed Photos template with
/// googlePhotosSig 24bb24c0…, service oauth2:openid + the two auth scopes,
/// lang en_US, sdk_version 33, google_play_services_version 240913000).
const EMBEDDED_SETUP_CREDENTIAL: &str = "Email=person%40example.com&Token=aas_et%2Ftest-master-token&androidId=0123456789abcdef&app=com.google.android.apps.photos&callerPkg=com.google.android.apps.photos&callerSig=24bb24c05e47e0aefa68a58a766179d9b613a600&client_sig=24bb24c05e47e0aefa68a58a766179d9b613a600&device_country=us&google_play_services_version=240913000&lang=en_US&oauth2_foreground=1&operatorCountry=us&sdk_version=33&service=oauth2%3Aopenid+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fmobileapps.native+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fphotos.native&source=android";

#[test]
fn embedded_setup_credential_parses_and_roundtrips_canonically() {
    let cred = Credential::parse(EMBEDDED_SETUP_CREDENTIAL).expect("parse Embedded Setup credential");
    // [A1] Canonical re-format is byte-identical to Go's Encode() output.
    assert_eq!(cred.to_string(), EMBEDDED_SETUP_CREDENTIAL);
    // Idempotence: parsing the re-formatted string changes nothing.
    let again = Credential::parse(&cred.to_string()).expect("re-parse");
    assert_eq!(again.to_string(), EMBEDDED_SETUP_CREDENTIAL);
}

#[test]
fn embedded_setup_credential_never_needs_token_binding() {
    // Scoping fact (study RESEARCH_3/5): buildGooglePhotosCredential mints a
    // credential with neither assertion_jwt nor check_tb_upgrade_eligible,
    // so Embedded Setup accounts never trigger token binding in v1.
    let cred = Credential::parse(EMBEDDED_SETUP_CREDENTIAL).expect("parse");
    assert!(!cred.needs_token_binding());
}

#[test]
fn token_binding_flag_from_assertion_jwt() {
    // core/configmanager.go credentialNeedsTokenBinding: assertion_jwt
    // present (and no alias stored) → binding required.
    let raw = format!("{EMBEDDED_SETUP_CREDENTIAL}&assertion_jwt=eyJhbGciOiJFUzI1NiJ9.payload.sig");
    let cred = Credential::parse(&raw).expect("parse");
    assert!(cred.needs_token_binding());
}

#[test]
fn token_binding_flag_from_check_tb_upgrade_eligible() {
    let raw = format!("{EMBEDDED_SETUP_CREDENTIAL}&check_tb_upgrade_eligible=1");
    let cred = Credential::parse(&raw).expect("parse");
    assert!(cred.needs_token_binding());
}

#[test]
fn token_binding_alias_already_stored_clears_the_flag() {
    // If token_binding_alias is already present the credential is usable:
    // credentialNeedsTokenBinding returns false even when the trigger keys
    // are also present.
    let raw = format!(
        "{EMBEDDED_SETUP_CREDENTIAL}&assertion_jwt=eyJ.payload.sig&token_binding_alias=auth_account%3Aecdsa_keypair%3AQUJD"
    );
    let cred = Credential::parse(&raw).expect("parse");
    assert!(!cred.needs_token_binding());

    let alias_only = format!("{EMBEDDED_SETUP_CREDENTIAL}&token_binding_alias=auth_account%3Aecdsa_keypair%3AQUJD");
    let cred = Credential::parse(&alias_only).expect("parse");
    assert!(!cred.needs_token_binding());
}

#[test]
fn unknown_keys_survive_roundtrip() {
    // [A2] core/api.go getAuthToken copies ALL parsed keys into the bearer
    // request (deleting only it_caveat_types/assertion_jwt and setting
    // app/callerPkg), so dropping unknown keys at parse time would change
    // wire behaviour.
    let raw = format!("{EMBEDDED_SETUP_CREDENTIAL}&some_future_key=some%20value");
    let cred = Credential::parse(&raw).expect("parse");
    assert!(cred.to_string().contains("some_future_key=some+value"));
}

#[test]
fn percent_decoded_values_roundtrip() {
    // A master token containing '=' and '/' (see googleauth_test.go:
    // masterToken "oauth2rt_1/master=value=with=equals") must survive intact:
    // in the credential string it arrives escaped, and re-formatting must
    // re-escape it identically.
    let raw = "Email=person%40example.com&Token=oauth2rt_1%2Fmaster%3Dvalue%3Dwith%3Dequals&androidId=0123456789abcdef";
    let cred = Credential::parse(raw).expect("parse");
    assert_eq!(cred.to_string(), raw);
    assert!(!cred.needs_token_binding());
}

#[test]
fn invalid_percent_escape_is_an_error() {
    // [A3] Go url.ParseQuery rejects invalid escapes; a silent lossy parse
    // would corrupt the replayed auth request.
    assert!(Credential::parse("Email=person%zz@example.com&Token=x").is_err());
    assert!(Credential::parse("Email=person%4@example.com&Token=x").is_err());
}

#[test]
fn looks_like_auth_string_cases_from_go_tests() {
    // Port of TestLooksLikeAuthString (core/googleauth_test.go) onto the
    // closest contract surface: a value is a raw credential iff it parses
    // AND carries both Email and Token. [A4] Assumes parse validates the
    // presence of Email and Token; if the credential agent keeps parse
    // purely syntactic, move the Email/Token presence checks to the config
    // AddCredentials equivalent (Go validates required fields there:
    // androidId, app, client_sig, Email, Token, lang, service) and drop
    // the two negative assertions below to the config tests.
    assert!(Credential::parse("androidId=1&Email=a%40x.com&Token=t").is_ok());
    assert!(Credential::parse("  Email=a%40x.com&Token=t  ").is_ok());
    assert!(Credential::parse("Email=a%40x.com").is_err()); // no Token
    assert!(Credential::parse("oauth_token=<redacted>").is_err()); // an oauth_token, not a credential
    assert!(Credential::parse("").is_err());
}
