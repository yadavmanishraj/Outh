//! The Android credential string — port of the credential handling in
//! gotohp `core/googleauth.go` and `core/configmanager.go`.
//!
//! A credential is a single URL query string (`url.Values.Encode()` output
//! in Go, parsed back with `url.ParseQuery`): keys sorted on encode, values
//! query-escaped (space → `+`, everything outside `[A-Za-z0-9-_.~]`
//! percent-escaped). The master token lives under the `Token` key, the
//! account email under `Email`, the device id under `androidId`.
//!
//! One Photos credential is *minted* by the Embedded Setup flow
//! (`buildGooglePhotosCredential` in googleauth.go) from exactly 16 fields;
//! imported credentials come from captured Play Services auth requests and
//! may carry extra keys (`assertion_jwt`, `check_tb_upgrade_eligible`,
//! `token_binding_alias`, …), which are preserved verbatim.
//!
//! Token material must never be printed: [`Credential`] implements a
//! redacted [`fmt::Debug`] and deliberately does **not** implement
//! `Display`.

use std::fmt;

use crate::{Error, Result};

/// `googlePhotosPackage` (googleauth.go) — also the `app`/`callerPkg` value
/// the bearer exchange forces (api.go `getAuthToken`).
pub(crate) const GOOGLE_PHOTOS_PACKAGE: &str = "com.google.android.apps.photos";
/// `googlePhotosSig` (googleauth.go) — signing-cert hash of the Photos app.
pub(crate) const GOOGLE_PHOTOS_SIG: &str = "24bb24c05e47e0aefa68a58a766179d9b613a600";
/// `googlePlayServicesSig` (googleauth.go) — signing-cert hash of Play
/// Services; sent as `client_sig`/`callerSig` by the Embedded Setup exchange.
pub(crate) const GOOGLE_PLAY_SERVICES_SIG: &str = "38918a453d07199354f8b19af05ec6562ced5788";
/// `googlePhotosService` (googleauth.go) — the OAuth scope string baked
/// into every minted Photos credential.
pub(crate) const GOOGLE_PHOTOS_SERVICE: &str =
    "oauth2:openid https://www.googleapis.com/auth/mobileapps.native https://www.googleapis.com/auth/photos.native";

/// Fields a raw imported credential must carry (configmanager.go
/// `AddCredentials.requiredFields`).
pub const REQUIRED_IMPORT_FIELDS: [&str; 7] = [
    "androidId",
    "app",
    "client_sig",
    "Email",
    "Token",
    "lang",
    "service",
];

/// A parsed Android credential string. Field order is preserved from
/// parsing; serialization is canonical (Go `url.Values.Encode()` sorts by
/// key), so `Credential::parse(s)?.to_string()` is a stable normal form.
#[derive(Clone)]
pub struct Credential {
    pairs: Vec<(String, String)>,
}

/// Go's `url.Values` is a map: two credentials are equal when they carry
/// the same key/value pairs, regardless of pair order. (Serialization is
/// canonical-sorted, so a parsed round-trip yields sorted pairs while
/// `build_photos` yields template order — order must not affect equality.)
impl PartialEq for Credential {
    fn eq(&self, other: &Self) -> bool {
        let mut a: Vec<&(String, String)> = self.pairs.iter().collect();
        let mut b: Vec<&(String, String)> = other.pairs.iter().collect();
        a.sort();
        b.sort();
        a == b
    }
}

impl Eq for Credential {}

impl Credential {
    /// Port of `url.ParseQuery`: pairs split on `&`, key/value split on the
    /// first `=`, `+` decodes to a space, `%XX` decodes a byte. A pair
    /// containing `;` or a malformed `%` escape is an error, as in Go.
    pub fn parse(s: &str) -> Result<Credential> {
        Ok(Credential {
            pairs: parse_query(s)?,
        })
    }

    /// Canonical serialization (Go `url.Values.Encode()`).
    // Shadows the Display-based ToString by design: there is intentionally
    // no Display impl, so this inherent method is the only way to render a
    // credential, and it is only called to persist or send it.
    #[allow(clippy::inherent_to_string)]
    pub fn to_string(&self) -> String {
        encode_query(&self.pairs)
    }

    /// First value for `key`, or `""` when absent (Go `url.Values.Get`).
    pub fn get(&self, key: &str) -> &str {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    }

    /// Replace all values for `key` with a single one (Go `url.Values.Set`).
    pub fn set(&mut self, key: &str, value: &str) {
        let first = self.pairs.iter().position(|(k, _)| k == key);
        self.pairs.retain(|(k, _)| k != key);
        match first {
            Some(pos) => self
                .pairs
                .insert(pos.min(self.pairs.len()), (key.to_string(), value.to_string())),
            None => self.pairs.push((key.to_string(), value.to_string())),
        }
    }

    /// Remove every value for `key` (Go `url.Values.Del`).
    pub fn del(&mut self, key: &str) {
        self.pairs.retain(|(k, _)| k != key);
    }

    /// All key/value pairs, in stored order.
    pub fn pairs(&self) -> &[(String, String)] {
        &self.pairs
    }

    /// Account email (`Email` key).
    pub fn email(&self) -> &str {
        self.get("Email")
    }

    /// Long-lived Play Services master token (`Token` key, `aas_et/…`).
    pub fn master_token(&self) -> &str {
        self.get("Token")
    }

    /// Android device id (`androidId` key).
    pub fn android_id(&self) -> &str {
        self.get("androidId")
    }

    /// Token-binding key alias (`token_binding_alias` key), if one was
    /// imported/extracted for this credential.
    pub fn token_binding_alias(&self) -> &str {
        self.get("token_binding_alias")
    }

    /// Port of `credentialNeedsTokenBinding` (configmanager.go): a
    /// credential needs token binding when it carries `assertion_jwt` or
    /// `check_tb_upgrade_eligible` and has no binding alias yet.
    /// Embedded-Setup credentials never carry those flags and never need
    /// binding; v1 does not implement binding, so the UI badges these.
    pub fn needs_token_binding(&self) -> bool {
        if !self.token_binding_alias().is_empty() {
            return false;
        }
        !self.get("assertion_jwt").is_empty()
            || !self.get("check_tb_upgrade_eligible").is_empty()
    }

    /// Port of `buildGooglePhotosCredential` (googleauth.go): the credential
    /// template minted from an Embedded Setup exchange — fresh random
    /// `androidId`, Photos package + cert sig, fixed locale/service fields.
    pub fn build_photos(email: &str, master_token: &str, android_id: &str) -> Credential {
        let pairs: Vec<(String, String)> = [
            ("androidId", android_id),
            ("app", GOOGLE_PHOTOS_PACKAGE),
            ("callerPkg", GOOGLE_PHOTOS_PACKAGE),
            ("callerSig", GOOGLE_PHOTOS_SIG),
            ("client_sig", GOOGLE_PHOTOS_SIG),
            ("device_country", "us"),
            ("Email", email),
            ("google_play_services_version", "240913000"),
            ("lang", "en_US"),
            ("oauth2_foreground", "1"),
            ("operatorCountry", "us"),
            ("sdk_version", "33"),
            ("service", GOOGLE_PHOTOS_SERVICE),
            ("source", "android"),
            ("Token", master_token),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        Credential { pairs }
    }

    /// Port of the validation in `AddCredentials` (configmanager.go): every
    /// field in [`REQUIRED_IMPORT_FIELDS`] must be present and non-empty,
    /// and the email must be non-empty (implied by the field check, kept for
    /// message parity).
    pub fn validate_import(&self) -> Result<()> {
        let missing: Vec<&str> = REQUIRED_IMPORT_FIELDS
            .iter()
            .copied()
            .filter(|f| self.get(f).is_empty())
            .collect();
        if !missing.is_empty() {
            // Go formats the slice with %v: "[a b c]".
            return Err(Error::Auth(format!(
                "auth string missing required fields: [{}]",
                missing.join(" ")
            )));
        }
        if self.email().is_empty() {
            return Err(Error::Auth("email cannot be empty".into()));
        }
        Ok(())
    }

    /// Port of `LooksLikeAuthString` (configmanager.go): a raw credential
    /// (parses as a query string with non-empty `Email` and `Token`) rather
    /// than an Embedded Setup `oauth_token`.
    pub fn looks_like(s: &str) -> bool {
        let value = s.trim();
        if !value.contains('=') {
            return false;
        }
        match Credential::parse(value) {
            Ok(cred) => !cred.email().is_empty() && !cred.master_token().is_empty(),
            Err(_) => false,
        }
    }
}

impl fmt::Debug for Credential {
    /// Redacted: identifies the account, never the token material.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credential")
            .field("email", &self.email())
            .field("android_id", &self.android_id())
            .field("needs_token_binding", &self.needs_token_binding())
            .field("fields", &self.pairs.len())
            .field("master_token", &"<redacted>")
            .finish()
    }
}

/// Parse a URL query string (Go `url.ParseQuery`). Error messages never
/// include the input — it may contain a master token.
pub(crate) fn parse_query(s: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    if s.is_empty() {
        return Ok(out);
    }
    for pair in s.split('&') {
        if pair.contains(';') {
            return Err(Error::Auth("invalid auth string format".into()));
        }
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        out.push((query_decode(key)?, query_decode(value)?));
    }
    Ok(out)
}

/// Serialize pairs exactly like Go `url.Values.Encode()`: keys sorted
/// (byte order), each key/value query-escaped.
pub(crate) fn encode_query(pairs: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = pairs.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::new();
    for (i, (k, v)) in sorted.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&query_escape(k));
        out.push('=');
        out.push_str(&query_escape(v));
    }
    out
}

/// Go `url.QueryEscape`: space → `+`; bytes outside `[A-Za-z0-9-_.~]`
/// percent-escaped with uppercase hex.
fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
                out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap().to_ascii_uppercase());
            }
        }
    }
    out
}

/// Go `url.QueryUnescape`: `+` → space, `%XX` → byte. The decoded bytes are
/// interpreted as UTF-8 lossily (Go strings tolerate arbitrary bytes; real
/// credentials are ASCII, so this is a non-issue in practice).
fn query_decode(s: &str) -> Result<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                if i + 2 >= bytes.len() {
                    return Err(Error::Auth("invalid auth string format".into()));
                }
                let hi = hex_value(bytes[i + 1])?;
                let lo = hex_value(bytes[i + 2])?;
                out.push(hi * 16 + lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn hex_value(b: u8) -> Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(Error::Auth("invalid auth string format".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The old-credential shape used by googleauth_test.go's replacement test.
    const OLD_STYLE: &str = "Email=person%40example.com&Token=old-token&androidId=fedcba9876543210";

    #[test]
    fn parse_extracts_fields_and_get_defaults_empty() {
        let cred = Credential::parse(OLD_STYLE).unwrap();
        assert_eq!(cred.email(), "person@example.com");
        assert_eq!(cred.master_token(), "old-token");
        assert_eq!(cred.android_id(), "fedcba9876543210");
        assert_eq!(cred.get("nope"), "");
    }

    #[test]
    fn build_photos_matches_go_template_and_roundtrips() {
        // buildGooglePhotosCredential(email, masterToken, androidID) from
        // googleauth_test.go: Token must survive intact even with '=' and
        // '/' inside it (Go case: "oauth2rt_1/master=value=with=equals").
        let master = "oauth2rt_1/master=value=with=equals";
        let cred = Credential::build_photos("person@example.com", master, "0123456789abcdef");
        assert_eq!(cred.email(), "person@example.com");
        assert_eq!(cred.master_token(), master);
        assert_eq!(cred.android_id(), "0123456789abcdef");
        assert_eq!(cred.get("app"), GOOGLE_PHOTOS_PACKAGE);
        assert_eq!(cred.get("callerPkg"), GOOGLE_PHOTOS_PACKAGE);
        assert_eq!(cred.get("callerSig"), GOOGLE_PHOTOS_SIG);
        assert_eq!(cred.get("client_sig"), GOOGLE_PHOTOS_SIG);
        assert_eq!(cred.get("service"), GOOGLE_PHOTOS_SERVICE);
        assert_eq!(cred.get("sdk_version"), "33");
        assert_eq!(cred.get("google_play_services_version"), "240913000");
        assert_eq!(cred.get("lang"), "en_US");
        // Embedded-Setup credentials never need token binding.
        assert!(!cred.needs_token_binding());

        let encoded = cred.to_string();
        let reparsed = Credential::parse(&encoded).unwrap();
        assert_eq!(reparsed, cred);
        assert_eq!(reparsed.master_token(), master);
    }

    #[test]
    fn encode_is_canonical_sorted_like_go() {
        // url.Values.Encode() sorts keys byte-wise: uppercase before
        // lowercase, so Email < Token < androidId.
        let cred = Credential::parse(OLD_STYLE).unwrap();
        assert_eq!(
            cred.to_string(),
            "Email=person%40example.com&Token=old-token&androidId=fedcba9876543210"
        );
    }

    #[test]
    fn parse_rejects_bad_escape_and_semicolons() {
        assert!(Credential::parse("Email=a%zz&Token=t").is_err());
        assert!(Credential::parse("Email=a;Token=t").is_err());
        assert!(Credential::parse("Email=a%4").is_err());
    }

    #[test]
    fn needs_token_binding_port_of_go() {
        let plain = Credential::parse("Email=a%40x.com&Token=t").unwrap();
        assert!(!plain.needs_token_binding());

        let jwt = Credential::parse("Email=a%40x.com&Token=t&assertion_jwt=xyz").unwrap();
        assert!(jwt.needs_token_binding());

        let upgrade =
            Credential::parse("Email=a%40x.com&Token=t&check_tb_upgrade_eligible=1").unwrap();
        assert!(upgrade.needs_token_binding());

        // An alias already stored cancels the requirement.
        let aliased = Credential::parse(
            "Email=a%40x.com&Token=t&assertion_jwt=xyz&token_binding_alias=auth_account%3Aecdsa",
        )
        .unwrap();
        assert!(!aliased.needs_token_binding());
    }

    #[test]
    fn looks_like_auth_string_port_of_go_test() {
        // Cases from TestLooksLikeAuthString (configmanager/googleauth tests).
        assert!(Credential::looks_like("androidId=1&Email=a%40x.com&Token=t"));
        assert!(Credential::looks_like("  Email=a%40x.com&Token=t  "));
        assert!(!Credential::looks_like("Email=a%40x.com"));
        assert!(!Credential::looks_like("oauth_token=some-cookie-value"));
        assert!(!Credential::looks_like(
            "4/0AbCdEfGhIjKlMnOpQrStUvWxYz-1234567890abcdef"
        ));
        assert!(!Credential::looks_like(""));
    }

    #[test]
    fn validate_import_reports_missing_fields() {
        let full = Credential::build_photos("a@x.com", "t", "0123456789abcdef");
        assert!(full.validate_import().is_ok());
        let sparse = Credential::parse("Email=a%40x.com&Token=t").unwrap();
        let err = sparse.validate_import().unwrap_err().to_string();
        assert!(err.contains("missing required fields"), "{err}");
        assert!(err.contains("androidId"), "{err}");
        assert!(err.contains("service"), "{err}");
    }

    #[test]
    fn set_and_del_match_go_values_semantics() {
        let mut cred = Credential::parse(OLD_STYLE).unwrap();
        cred.set("Token", "new");
        assert_eq!(cred.master_token(), "new");
        assert_eq!(cred.pairs().iter().filter(|(k, _)| k == "Token").count(), 1);
        cred.del("Token");
        assert_eq!(cred.master_token(), "");
    }

    #[test]
    fn debug_output_is_redacted() {
        let cred = Credential::build_photos("a@x.com", "aas_et/SUPERSECRET", "0123456789abcdef");
        let rendered = format!("{:?}", cred);
        assert!(!rendered.contains("SUPERSECRET"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("a@x.com"), "{rendered}");
    }
}
