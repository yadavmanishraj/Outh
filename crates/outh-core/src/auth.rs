//! Embedded Setup auth — port of gotohp `core/googleauth.go`.
//!
//! The flow (the only Windows-native sign-in gotohp has): the user signs in
//! on Google's Embedded Setup page in a browser and copies the `oauth_token`
//! cookie value; this module exchanges it at
//! `https://android.clients.google.com/auth` — impersonating Play Services
//! (its cert sig `client_sig`/`callerSig`, literal
//! `droidguard_results=dummy123`, see googleauth.go) — for a long-lived
//! master token, mints the Photos credential template from it
//! ([`Credential::build_photos`]), validates the credential, and returns it.
//! Persistence is the caller's job ([`crate::config::ConfigService::upsert_credential`]),
//! split out the way Go's testable `addGoogleAccount` separates exchange,
//! validation and `upsertCredential`.
//!
//! Dependency injection mirrors `googleAccountAuthDependencies` in
//! googleauth_test.go: transport, Android-ID generator and credential
//! validator are traits/function values so tests run with no network.
//!
//! Token material never appears in error messages (a Go test asserts the
//! oauth cookie is absent from errors; the ported tests assert the same).

use std::collections::HashMap;
use std::sync::Arc;

use crate::credential::{
    encode_query, Credential, GOOGLE_PLAY_SERVICES_SIG,
};
use crate::http::build_auth_agent;
use crate::{Error, Result};

/// Port of `embeddedSetupAuthEndpoint` (googleauth.go).
pub const EMBEDDED_SETUP_AUTH_ENDPOINT: &str = "https://android.clients.google.com/auth";
/// Port of `googleAuthEmailHint` (googleauth.go): the placeholder `Email`
/// form field sent during the Embedded Setup exchange.
pub const GOOGLE_AUTH_EMAIL_HINT: &str = "oauth-token@example.com";
/// The bearer-exchange endpoint (api.go `getAuthToken`). The exchange and
/// the bearer request share the `Key=Value` response format parsed by
/// [`parse_auth_response`].
pub const GOOGLE_AUTH_ENDPOINT: &str = "https://android.googleapis.com/auth";

/// Successful Embedded Setup exchange: the account email *as returned by
/// Google* (never the hint) and the master token. Debug is redacted.
pub struct AuthExchange {
    email: String,
    master_token: String,
}

impl AuthExchange {
    pub fn email(&self) -> &str {
        &self.email
    }
    pub fn master_token(&self) -> &str {
        &self.master_token
    }
}

impl std::fmt::Debug for AuthExchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthExchange")
            .field("email", &self.email)
            .field("master_token", &"<redacted>")
            .finish()
    }
}

/// Parse a form-encoded Google auth response — port of
/// `parseGoogleAuthResponse` (googleauth.go), which api.go's bearer
/// exchange duplicates inline; both flows share this implementation.
///
/// Format: one `Key=Value` per line, split on the *first* `=` (values may
/// contain `=`), lines trimmed, lines without `=` skipped, later keys
/// overwrite earlier ones (Go map semantics).
pub fn parse_auth_response(body: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();
    for raw_line in body.split('\n') {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            result.insert(key.to_string(), value.to_string());
        }
    }
    result
}

/// Map a Google auth `Error=` code to our error type — port of
/// `googleAuthError` (googleauth.go). The two distinctions with dedicated
/// variants keep them; the rest keep Go's message text via `Error::Auth`.
pub fn google_auth_error(code: &str) -> Error {
    match code {
        // Go message: "Google rejected the oauth_token; obtain a fresh
        // cookie and try again" — carried by Error::BadAuthentication's UI
        // copy; the variant preserves the distinction, per CONTRACT.md.
        "BadAuthentication" => Error::BadAuthentication,
        // Go message: "Google requires a fresh Embedded Setup sign-in".
        "NeedsBrowser" => Error::NeedsBrowser,
        "MissingDroidguard" => {
            Error::Auth("Google rejected the device verification data".into())
        }
        other => Error::Auth(format!("Google authentication failed with {other}")),
    }
}

/// Port of `normalizeGoogleEmail` (googleauth.go): strict single-address
/// validation — the trimmed value must be exactly one addr-spec, no display
/// name, no group syntax. (Go uses `net/mail.ParseAddress`; this checks the
/// same invariants without a mail-parser dependency.)
pub fn normalize_google_email(value: &str) -> Result<String> {
    let value = value.trim();
    let invalid = Error::Auth("enter a valid Google account email address".into());
    if value.is_empty()
        || value.chars().any(|c| c.is_whitespace() || "<>\"'(),:;[]".contains(c))
        || value.matches('@').count() != 1
    {
        return Err(invalid);
    }
    let (local, domain) = value.split_once('@').unwrap();
    if local.is_empty()
        || domain.is_empty()
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.split('.').any(|label| label.is_empty())
    {
        return Err(invalid);
    }
    Ok(value.to_string())
}

/// Port of `normalizeOAuthToken` (googleauth.go): trim, accept a pasted
/// `oauth_token=…` cookie pair as well as the bare value, require
/// 16..=8192 chars and no CR/LF.
pub fn normalize_oauth_token(value: &str) -> Result<String> {
    let mut value = value.trim();
    if let Some(rest) = value.strip_prefix("oauth_token=") {
        value = rest;
    }
    if value.len() < 16 || value.len() > 8192 || value.contains(['\r', '\n']) {
        return Err(Error::Auth(
            "enter the oauth_token cookie value from Google Embedded Setup".into(),
        ));
    }
    Ok(value.to_string())
}

/// Port of `generateAndroidID` (googleauth.go): 8 random bytes, lowercase
/// hex (16 chars). Uses OS entropy where available (`/dev/urandom`);
/// otherwise the crate PRNG (http.rs). Go's `crypto/rand` failure path has
/// no realistic equivalent here, so this is infallible.
pub fn generate_android_id() -> String {
    let mut bytes = [0u8; 8];
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let _ = f.read_exact(&mut bytes);
        }
    }
    if bytes.iter().all(|&b| b == 0) {
        bytes = crate::http::next_random_u64().to_be_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Transport seam (Go: `*http.Client` in the dependencies struct). Implementations
/// POST a form and return `(status, body)`; HTTP error statuses are *not*
/// errors here — the exchange applies Go's status handling itself.
pub trait AuthTransport: Send + Sync {
    fn post_form(&self, url: &str, form: &[(String, String)]) -> Result<(u16, String)>;
}

/// Production transport over the auth agent (no redirects, 30 s cap,
/// TLS always verified — see [`crate::http::build_auth_agent`]).
pub struct UreqAuthTransport {
    proxy: String,
}

impl UreqAuthTransport {
    pub fn new(proxy: &str) -> Self {
        UreqAuthTransport {
            proxy: proxy.to_string(),
        }
    }
}

impl AuthTransport for UreqAuthTransport {
    fn post_form(&self, url: &str, form: &[(String, String)]) -> Result<(u16, String)> {
        let agent = build_auth_agent(&self.proxy)?;
        let body = encode_query(form);
        let mut response = agent
            .post(url)
            .header("Accept-Encoding", "identity")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("User-Agent", "GoogleAuth/1.4")
            .send(body.as_str())
            .map_err(|e| Error::Auth(format!("contact Google authentication: {e}")))?;
        let status = response.status().as_u16();
        // Go caps the response read at 64 KiB (io.LimitReader).
        let mut bytes = response
            .body_mut()
            .read_to_vec()
            .map_err(|e| Error::Auth(format!("read Google authentication response: {e}")))?;
        bytes.truncate(64 * 1024);
        Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
    }
}

/// Credential-validation seam (Go: `validateCredential` dependency).
/// Validation runs *before* a credential is ever persisted.
pub trait CredentialValidator: Send + Sync {
    fn validate(&self, credential: &Credential) -> Result<()>;
}

/// Live validator — port of `validateGooglePhotosCredential`
/// (googleauth.go): build a Photos client from the credential, and require
/// an empty-hash lookup to return no media key (which also forces a bearer
/// exchange inside the client, as Go's explicit `BearerToken()` call did).
pub struct LiveCredentialValidator {
    proxy: String,
}

impl LiveCredentialValidator {
    pub fn new(proxy: &str) -> Self {
        LiveCredentialValidator {
            proxy: proxy.to_string(),
        }
    }
}

impl CredentialValidator for LiveCredentialValidator {
    fn validate(&self, credential: &Credential) -> Result<()> {
        let options = crate::config::ApiOptions {
            proxy: self.proxy.clone(),
            saver: false,
            use_quota: false,
        };
        let client = crate::api::PhotosClient::new(credential.clone(), options)?;
        // Go: FindRemoteMediaByHash(make([]byte, 20)) — an all-zero hash
        // must not match anything.
        match client.find_remote_media_by_hash(&[0u8; 20])? {
            None => Ok(()),
            Some(_) => Err(Error::Auth(
                "unexpected media match for validation hash".into(),
            )),
        }
    }
}

/// A validator that accepts everything — tests and offline plumbing only.
pub struct AcceptingValidator;

impl CredentialValidator for AcceptingValidator {
    fn validate(&self, _credential: &Credential) -> Result<()> {
        Ok(())
    }
}

/// Android-ID generator seam (Go: `generateAndroidID` dependency).
pub type AndroidIdGenerator = Arc<dyn Fn() -> String + Send + Sync>;

/// Embedded Setup auth service — the testable core of Go's
/// `ConfigManager.AddGoogleAccount` / `addGoogleAccount`.
pub struct EmbeddedSetupAuth {
    endpoint: String,
    transport: Arc<dyn AuthTransport>,
    generate_android_id: AndroidIdGenerator,
    validator: Arc<dyn CredentialValidator>,
}

impl EmbeddedSetupAuth {
    /// Production wiring: real transport + live validation, proxy taken
    /// from preferences (Go reads `AppConfig.Preferences.Proxy`).
    pub fn new(proxy: &str) -> Self {
        EmbeddedSetupAuth {
            endpoint: EMBEDDED_SETUP_AUTH_ENDPOINT.to_string(),
            transport: Arc::new(UreqAuthTransport::new(proxy)),
            generate_android_id: Arc::new(generate_android_id),
            validator: Arc::new(LiveCredentialValidator::new(proxy)),
        }
    }

    /// Dependency-injected wiring — the counterpart of Go's
    /// `googleAccountAuthDependencies` used throughout googleauth_test.go.
    pub fn with_dependencies(
        endpoint: String,
        transport: Arc<dyn AuthTransport>,
        generate_android_id: AndroidIdGenerator,
        validator: Arc<dyn CredentialValidator>,
    ) -> Self {
        EmbeddedSetupAuth {
            endpoint,
            transport,
            generate_android_id,
            validator,
        }
    }

    /// Exchange an Embedded Setup `oauth_token` for a validated Photos
    /// credential — port of `addGoogleAccount` minus persistence:
    /// normalize → generate Android ID → exchange → build credential →
    /// validate. The caller persists the returned credential
    /// (Go's `upsertCredential` step lives in
    /// [`crate::config::ConfigService::upsert_credential`]).
    pub fn exchange_oauth_token(&self, oauth_token: &str) -> Result<Credential> {
        let oauth_token = normalize_oauth_token(oauth_token)?;
        let android_id = (self.generate_android_id)();
        let exchange = self.exchange(&oauth_token, &android_id)?;
        let credential =
            Credential::build_photos(exchange.email(), exchange.master_token(), &android_id);
        self.validator.validate(&credential).map_err(|e| match e {
            // Keep dedicated auth variants distinguishable through the wrap.
            Error::BadAuthentication | Error::NeedsBrowser => e,
            other => Error::Auth(format!(
                "Google Photos rejected the new credential: {other}"
            )),
        })?;
        Ok(credential)
    }

    /// Port of `exchangeEmbeddedSetupToken` (googleauth.go): the form POST
    /// and response handling, without credential construction.
    pub fn exchange(&self, oauth_token: &str, android_id: &str) -> Result<AuthExchange> {
        // Field set mirrors googleauth.go's url.Values exactly, incl. the
        // Embedded Setup markers and the literal dummy DroidGuard value
        // Google currently tolerates (see GOTOH_STUDY risk R-4).
        let sig = GOOGLE_PLAY_SERVICES_SIG;
        let form: Vec<(String, String)> = [
            ("accountType", "HOSTED_OR_GOOGLE"),
            ("Email", GOOGLE_AUTH_EMAIL_HINT),
            ("has_permission", "1"),
            ("add_account", "1"),
            ("ACCESS_TOKEN", "1"),
            ("Token", oauth_token),
            ("service", "ac2dm"),
            ("source", "android"),
            ("androidId", android_id),
            ("device_country", "us"),
            ("operatorCountry", "us"),
            ("lang", "en"),
            ("sdk_version", "17"),
            ("google_play_services_version", "240913000"),
            ("client_sig", sig),
            ("callerSig", sig),
            ("droidguard_results", "dummy123"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let (status, body) = self
            .transport
            .post_form(&self.endpoint, &form)
            .map_err(|e| match e {
                Error::Auth(msg) => Error::Auth(msg),
                other => Error::Auth(format!("contact Google authentication: {other}")),
            })?;
        if !(200..300).contains(&status) {
            // Go: any non-2xx (including the redirect responses the auth
            // transport refuses to follow) is reported by status alone —
            // never with the body/Location, which can carry token material.
            return Err(Error::Auth(format!(
                "Google authentication returned HTTP {status}"
            )));
        }
        let values = parse_auth_response(&body);
        if let Some(code) = values.get("Error") {
            if !code.is_empty() {
                return Err(google_auth_error(code));
            }
        }
        let master_token = values.get("Token").cloned().unwrap_or_default();
        if master_token.is_empty() {
            return Err(Error::Auth(
                "Google authentication response did not contain a master token".into(),
            ));
        }
        let email = normalize_google_email(values.get("Email").map(|s| s.as_str()).unwrap_or(""))
            .map_err(|_| {
                Error::Auth(
                    "Google authentication response did not contain a valid account email"
                        .into(),
                )
            })?;
        Ok(AuthExchange {
            email,
            master_token,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Fake transport: records the last request, replies with a canned
    /// (status, body) — the httptest.Server counterpart from the Go tests.
    struct FakeTransport {
        response: (u16, String),
        captured: Mutex<Option<(String, Vec<(String, String)>)>>,
    }

    impl FakeTransport {
        fn new(status: u16, body: &str) -> Arc<Self> {
            Arc::new(FakeTransport {
                response: (status, body.to_string()),
                captured: Mutex::new(None),
            })
        }
        fn form(&self) -> Vec<(String, String)> {
            self.captured.lock().unwrap().clone().unwrap().1
        }
    }

    impl AuthTransport for FakeTransport {
        fn post_form(&self, url: &str, form: &[(String, String)]) -> Result<(u16, String)> {
            *self.captured.lock().unwrap() = Some((url.to_string(), form.to_vec()));
            Ok(self.response.clone())
        }
    }

    struct FakeValidator {
        result: Result<()>,
        seen: Mutex<Vec<Credential>>,
    }

    impl FakeValidator {
        fn ok() -> Arc<Self> {
            Arc::new(FakeValidator {
                result: Ok(()),
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    impl CredentialValidator for FakeValidator {
        fn validate(&self, credential: &Credential) -> Result<()> {
            self.seen.lock().unwrap().push(credential.clone());
            match &self.result {
                Ok(()) => Ok(()),
                Err(e) => Err(Error::Auth(e.to_string())),
            }
        }
    }

    fn auth_with(
        transport: Arc<FakeTransport>,
        validator: Arc<FakeValidator>,
    ) -> EmbeddedSetupAuth {
        EmbeddedSetupAuth::with_dependencies(
            "http://auth.invalid/".to_string(),
            transport,
            Arc::new(|| "0123456789abcdef".to_string()),
            validator,
        )
    }

    // Port of TestExchangeEmbeddedSetupToken (googleauth_test.go): the
    // master token "oauth2rt_1/master=value=with=equals" must be parsed
    // intact (split on first '=') and the request form must carry the
    // Embedded Setup fields.
    #[test]
    fn exchange_parses_master_token_with_equals_intact() {
        let transport = FakeTransport::new(
            200,
            "Token=oauth2rt_1/master=value=with=equals\nEmail=person@example.com\n",
        );
        let auth = auth_with(transport.clone(), FakeValidator::ok());
        let exchange = auth
            .exchange("test-oauth-cookie-value", "0123456789abcdef")
            .unwrap();
        assert_eq!(exchange.master_token(), "oauth2rt_1/master=value=with=equals");
        assert_eq!(exchange.email(), "person@example.com");

        let form: HashMap<String, String> = transport.form().into_iter().collect();
        assert_eq!(form["ACCESS_TOKEN"], "1");
        assert_eq!(form["Email"], GOOGLE_AUTH_EMAIL_HINT);
        assert_eq!(form["Token"], "test-oauth-cookie-value");
        assert_eq!(form["androidId"], "0123456789abcdef");
        assert_eq!(form["client_sig"], GOOGLE_PLAY_SERVICES_SIG);
        assert_eq!(form["callerSig"], GOOGLE_PLAY_SERVICES_SIG);
        assert_eq!(form["google_play_services_version"], "240913000");
        assert_eq!(form["service"], "ac2dm");
        assert_eq!(form["droidguard_results"], "dummy123");
    }

    // Port of TestExchangeEmbeddedSetupTokenRequiresGoogleResponseEmail.
    #[test]
    fn exchange_requires_response_email() {
        let transport = FakeTransport::new(200, "Token=test-master-token\n");
        let auth = auth_with(transport, FakeValidator::ok());
        let err = auth
            .exchange("test-oauth-cookie-value", "0123456789abcdef")
            .unwrap_err();
        assert!(err.to_string().contains("valid account email"), "{err}");
    }

    // Port of
    // TestExchangeEmbeddedSetupTokenRejectsBadAuthenticationWithoutLeakingCookie:
    // the variant must surface and the oauth cookie must not appear in the
    // error text.
    #[test]
    fn bad_authentication_maps_to_variant_and_never_leaks_cookie() {
        let transport = FakeTransport::new(200, "Error=BadAuthentication\n");
        let auth = auth_with(transport, FakeValidator::ok());
        let err = auth
            .exchange("test-secret-oauth-cookie", "0123456789abcdef")
            .unwrap_err();
        assert!(matches!(err, Error::BadAuthentication), "{err:?}");
        assert!(!err.to_string().contains("test-secret-oauth-cookie"));
    }

    #[test]
    fn error_code_mapping_port_of_google_auth_error() {
        assert!(matches!(
            google_auth_error("NeedsBrowser"),
            Error::NeedsBrowser
        ));
        let droid = google_auth_error("MissingDroidguard");
        assert!(droid.to_string().contains("device verification"), "{droid}");
        let other = google_auth_error("UserCancel");
        assert!(other.to_string().contains("UserCancel"), "{other}");
    }

    // The redirect guarantee at the exchange layer: a 307 (which the auth
    // transport will not follow) is an error naming the status, mirroring
    // TestGoogleAuthenticationRejectsRedirects' observable behaviour.
    #[test]
    fn redirect_status_is_an_error_naming_the_status() {
        let transport = FakeTransport::new(307, "");
        let auth = auth_with(transport, FakeValidator::ok());
        let err = auth
            .exchange("test-oauth-cookie-value", "0123456789abcdef")
            .unwrap_err();
        assert!(err.to_string().contains("307"), "{err}");
    }

    // Port of TestAddGoogleAccountUsesGoogleEmailAndReplacesCredential
    // (validation half): the built credential uses Google's email, the
    // generated Android ID and the exchanged master token, and validation
    // runs before the credential is returned for persistence.
    #[test]
    fn exchange_oauth_token_builds_and_validates_credential() {
        let transport = FakeTransport::new(
            200,
            "Token=oauth2rt_1/generated-master-token\nEmail=person@example.com\n",
        );
        let validator = FakeValidator::ok();
        let auth = auth_with(transport, validator.clone());
        let cred = auth.exchange_oauth_token("test-oauth-cookie-value").unwrap();
        assert_eq!(cred.email(), "person@example.com");
        assert_eq!(cred.master_token(), "oauth2rt_1/generated-master-token");
        assert_eq!(cred.android_id(), "0123456789abcdef");
        let seen = validator.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0], cred);
    }

    // Port of TestAddGoogleAccountDoesNotPersistFailedValidation: a failed
    // validation must fail the whole exchange (nothing is returned to save).
    #[test]
    fn failed_validation_fails_exchange() {
        let transport = FakeTransport::new(
            200,
            "Token=test-master-token\nEmail=person@example.com\n",
        );
        let validator = Arc::new(FakeValidator {
            result: Err(Error::Auth("validation failed".into())),
            seen: Mutex::new(Vec::new()),
        });
        let auth = auth_with(transport, validator);
        let err = auth.exchange_oauth_token("test-oauth-cookie-value").unwrap_err();
        assert!(err.to_string().contains("rejected the new credential"), "{err}");
    }

    // Port of TestGenerateAndroidID: 16 lowercase hex chars.
    #[test]
    fn generated_android_id_format() {
        let id = generate_android_id();
        assert_eq!(id.len(), 16, "{id}");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "{id}"
        );
    }

    #[test]
    fn oauth_token_normalization_port() {
        assert_eq!(
            normalize_oauth_token("  abcdefghijklmnop  ").unwrap(),
            "abcdefghijklmnop"
        );
        assert_eq!(
            normalize_oauth_token("oauth_token=abcdefghijklmnop").unwrap(),
            "abcdefghijklmnop"
        );
        assert!(normalize_oauth_token("short").is_err());
        assert!(normalize_oauth_token("abcdefghijklmnop\nx").is_err());
    }

    #[test]
    fn parse_auth_response_port() {
        // Go parseGoogleAuthResponse: first '=' splits, junk lines skipped,
        // trailing whitespace/CR trimmed, later keys win.
        let map = parse_auth_response("Auth=abc=def\n\nnonsense\nExpiry=123\r\nAuth=final\n");
        assert_eq!(map["Auth"], "final");
        assert_eq!(map["Expiry"], "123");
        assert!(!map.contains_key("nonsense"));
    }
}
