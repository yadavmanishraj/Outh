//! Port of gotohp `core/api.go` (843 lines, HEAD 97a5dc0) — the Google
//! Photos private-protocol client, plus the retry helpers of
//! `core/httpclient.go` and the counting reader of
//! `core/progress_reader.go` where they belong to this client's calls.
//!
//! Protocol summary (see ~/workspace/sdlc/gotohp/GOTOH_STUDY.md): the client
//! impersonates the Android Photos app (Pixel identity, Cronet UA), replays
//! an Android master-token credential at `android.googleapis.com/auth` for
//! a bearer, dedups by SHA-1 via HashCheck, opens a Scotty upload session,
//! PUTs the raw file in ONE chunked request (not resumable; a retry
//! re-streams the whole file), and commits the opaque Scotty finalize
//! token. Commit responses are decoded as `CreateMediaItemsResponse`
//! (the generated `CommitUploadResponse` schema is dead — never used).
//!
//! Deliberate deviations from Go, per CONTRACT.md:
//! - The device profile is chosen IMMUTABLY in `PhotosClient::new`
//!   (Go mutates `Api.model` inside `CommitUpload`; RESEARCH_5 flagged the
//!   smell). Observable wire behaviour is identical, including the quirk
//!   that the User-Agent always names the Pixel XL: Go builds the UA in
//!   `newAPIFromCredential`, before Saver/UseQuota are applied.
//! - TLS certificate verification is NEVER disabled (Go disables it when
//!   a proxy is configured, core/httpclient.go — not ported).
//! - Redirects are never auto-followed (the bearer exchange carries a
//!   master token; gotohp core/googleauth.go rejects redirects for its
//!   auth clients for the same reason).
//! - Token binding is not implemented in v1 (CONTRACT decision 4): a
//!   credential carrying `token_binding_alias` cannot complete the
//!   bearer exchange, and a `TokenEncrypted=1` answer is an error,
//!   mirroring Go's behaviour when no binding session exists.
//! - Non-retryable commit errors are returned unwrapped (Go wraps them
//!   with "commit failed after N attempt(s):") so the
//!   `Error::CommitAmbiguous` distinction survives to the UI.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;

use crate::create_media_items::{
    build_live_photo_create_media_items_request, build_live_photo_reconcile_media_items_request,
    LivePhotoCommitPolicy, LivePhotoCreateRequest, LivePhotoReconcileRequest, UploadDeviceInfo,
};
use crate::credential::Credential;
use crate::protocol::{
    AddMediaToAlbum, AddMediaToAlbumField5, CommitToken, CommitUpload, CommitUploadField1,
    CommitUploadField4, CreateAlbum, CreateAlbumField6, CreateAlbumField7, CreateAlbumResponse,
    CreateMediaItemsResponse, DeviceInfo, GetUploadToken, HashCheck, RemoteMatches,
};
use crate::types::CancellationToken;
use crate::{Error, Result};

/// Per-run client options have ONE definition, in [`crate::config`]
/// (next to the preferences they derive from); re-exported here so
/// existing `api::ApiOptions` paths keep working.
pub use crate::config::ApiOptions;

/// The Scotty finalize token type lives in [`crate::protocol`];
/// re-exported here because this client's upload methods return it.
pub use crate::protocol::ScottyToken;

// ---------------------------------------------------------------------------
// Endpoint / RPC constants — ALL in one place so protocol drift (study
// risk R-3) is a one-file fix. The photosdata-pa path segment
// `6439526531001121323` is the app id; the trailing number is the RPC id.
// ---------------------------------------------------------------------------

/// gotohp core/api.go: photosCreateMediaItemsEndpoint host+app prefix.
pub const PHOTOS_DATA_PA_BASE: &str =
    "https://photosdata-pa.googleapis.com/6439526531001121323";

/// HashCheck (dedup) RPC — gotohp core/api.go FindRemoteMediaByHash.
pub const HASH_CHECK_ENDPOINT: &str =
    "https://photosdata-pa.googleapis.com/6439526531001121323/5084965799730810217";

/// Commit / CreateMediaItems RPC — gotohp core/api.go
/// `photosCreateMediaItemsEndpoint`, shared by single-file commits and
/// Live Photo create/reconcile.
pub const CREATE_MEDIA_ITEMS_ENDPOINT: &str =
    "https://photosdata-pa.googleapis.com/6439526531001121323/16538846908252377752";

/// CreateAlbum RPC — gotohp core/api.go CreateAlbum.
pub const CREATE_ALBUM_ENDPOINT: &str =
    "https://photosdata-pa.googleapis.com/6439526531001121323/8386163679468898444";

/// AddMediaToAlbum RPC — gotohp core/api.go AddMediaToAlbum.
pub const ADD_MEDIA_TO_ALBUM_ENDPOINT: &str =
    "https://photosdata-pa.googleapis.com/6439526531001121323/484917746253879292";

/// Scotty interactive upload endpoint — gotohp core/api.go GetUploadToken
/// (POST for the session) and UploadFileWithProgress (PUT
/// `?upload_id=<session id>` for the bytes).
pub const SCOTTY_INTERACTIVE_ENDPOINT: &str =
    "https://photos.googleapis.com/data/upload/uploadmedia/interactive";

/// Android master-token -> bearer exchange — gotohp core/api.go
/// getAuthToken.
pub const ANDROID_AUTH_ENDPOINT: &str = "https://android.googleapis.com/auth";

// ---------------------------------------------------------------------------
// Device identity constants — gotohp core/api.go newAPIFromCredential.
// ---------------------------------------------------------------------------

/// gotohp core/api.go: androidAPIVersion.
pub const ANDROID_API_VERSION: i64 = 28;
/// gotohp core/api.go: make.
pub const DEVICE_MAKE: &str = "Google";
/// gotohp core/api.go: default model (original-quality / "unlimited"
/// legacy-Pixel identity).
pub const MODEL_DEFAULT: &str = "Pixel XL";
/// gotohp core/api.go CommitUpload: Saver model.
pub const MODEL_SAVER: &str = "Pixel 2";
/// gotohp core/api.go CommitUpload: UseQuota model.
pub const MODEL_USE_QUOTA: &str = "Pixel 8";
/// gotohp core/api.go: clientVersionCode (Photos app version).
pub const CLIENT_VERSION_CODE: i64 = 49029607;
/// Android build id baked into both user agents — gotohp core/api.go.
pub const ANDROID_BUILD_ID: &str = "PQ2A.190205.001";
/// Storage policy for the default profile — gotohp core/api.go /
/// create_media_items.go buildLivePhotoCommitPolicy (legacy Pixel
/// original-quality quota exemption).
pub const STORAGE_POLICY_DEFAULT: i64 = 3;
/// Storage policy for Saver — gotohp core/create_media_items.go.
pub const STORAGE_POLICY_SAVER: i64 = 1;
/// CommitUpload `quality` for the default profile — gotohp core/api.go
/// CommitUpload (`qualityVal = 3`).
pub const COMMIT_QUALITY_DEFAULT: i64 = 3;
/// CommitUpload `quality` for Saver — gotohp core/api.go (`qualityVal = 1`).
pub const COMMIT_QUALITY_SAVER: i64 = 1;
/// Live Photo upload quality — gotohp core/create_media_items.go
/// buildLivePhotoCommitPolicy hardcodes `UploadQuality: 1`.
pub const LIVE_PHOTO_UPLOAD_QUALITY: i64 = 1;
/// Magic header on commit/album calls — gotohp core/api.go doCommitRequest,
/// CreateAlbum, AddMediaToAlbum (byte-exact, do not interpret).
pub const X_GOOG_EXT_173412678_BIN: &str = "CgcIAhClARgC";
/// Magic header on commit/album calls — gotohp core/api.go (byte-exact).
pub const X_GOOG_EXT_174067345_BIN: &str = "CgIIAg==";
/// gotohp core/api.go CommitUpload: `unknownInt` in Field4 of the commit
/// body (uninterpreted by upstream; preserved verbatim).
pub const COMMIT_UNKNOWN_INT: i64 = 46_000_000;
/// gotohp core/api.go CommitUpload: Field3 of the commit body.
pub const COMMIT_FIELD3: [u8; 2] = [1, 3];

// ---------------------------------------------------------------------------
// Retry constants — gotohp core/httpclient.go DefaultRetryConfig /
// CalculateBackoff / ShouldRetry.
// ---------------------------------------------------------------------------

/// gotohp core/httpclient.go: RetryConfig.MaxRetries.
pub const MAX_RETRIES: u32 = 3;
/// gotohp core/httpclient.go: RetryConfig.InitialDelay.
pub const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// gotohp core/httpclient.go: RetryConfig.MaxDelay.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Exponential backoff with up to 10% jitter — gotohp core/httpclient.go
/// CalculateBackoff: `delay = min(1s << attempt, 30s)`, plus
/// `rand[0, delay/10)`. The jitter source here is the system clock's
/// nanosecond field (no rand dependency in the workspace list); the
/// distribution differs from Go's, the bounds do not.
pub fn calculate_backoff(attempt: u32) -> Duration {
    let mut delay = INITIAL_BACKOFF;
    for _ in 0..attempt {
        delay = std::cmp::min(delay * 2, MAX_BACKOFF);
    }
    let base_ms = delay.as_millis() as u64;
    let jitter_window = base_ms / 10;
    if jitter_window == 0 {
        return delay;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    delay + Duration::from_millis(nanos % jitter_window)
}

/// gotohp core/httpclient.go ShouldRetry, status-code half: network errors
/// are handled by callers (they have no status); here, retry on 5xx and
/// 429 only.
pub fn should_retry_status(status: u16) -> bool {
    status >= 500 || status == 429
}

/// The immutable device profile chosen at client construction.
/// Selection mirrors Go's mutation order exactly (gotohp core/api.go
/// CommitUpload + core/create_media_items.go buildLivePhotoCommitPolicy):
/// start from the default Pixel XL; Saver switches model, storage policy
/// and commit quality; UseQuota then overrides ONLY the model (Pixel 8),
/// leaving a Saver policy/quality in place if both are set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceProfile {
    pub model: &'static str,
    pub make: &'static str,
    pub android_api_version: i64,
    pub client_version_code: i64,
    /// Storage policy used by Live Photo commits (3 default / 1 Saver).
    pub storage_policy: i64,
    /// `quality` field used by single-file CommitUpload (3 default / 1 Saver).
    pub commit_quality: i64,
}

impl DeviceProfile {
    pub fn select(saver: bool, use_quota: bool) -> Self {
        let mut profile = DeviceProfile {
            model: MODEL_DEFAULT,
            make: DEVICE_MAKE,
            android_api_version: ANDROID_API_VERSION,
            client_version_code: CLIENT_VERSION_CODE,
            storage_policy: STORAGE_POLICY_DEFAULT,
            commit_quality: COMMIT_QUALITY_DEFAULT,
        };
        if saver {
            profile.model = MODEL_SAVER;
            profile.storage_policy = STORAGE_POLICY_SAVER;
            profile.commit_quality = COMMIT_QUALITY_SAVER;
        }
        if use_quota {
            profile.model = MODEL_USE_QUOTA;
        }
        profile
    }

    fn device_info(&self) -> UploadDeviceInfo {
        UploadDeviceInfo {
            model: self.model.to_string(),
            make: self.make.to_string(),
            android_api_version: self.android_api_version,
        }
    }
}

/// An opened Scotty upload session. The session id comes ONLY from the
/// `X-GUploader-UploadID` response header (gotohp core/api.go
/// GetUploadToken) — it never appears in the response body.
#[derive(Clone, Debug)]
pub struct UploadSession {
    pub upload_id: String,
}

/// Metadata for a single-file commit — CONTRACT.md, api.rs section.
/// `taken_unix_secs` is the file mtime, or the filename-parsed capture
/// time when that preference is enabled (gotohp core/upload.go); 0 means
/// "use now", as in Go's CommitUpload.
#[derive(Clone, Debug)]
pub struct FileMeta {
    pub name: String,
    pub size: u64,
    pub sha1: [u8; 20],
    pub taken_unix_secs: i64,
}

/// Bearer cache, mirroring Go's `authResponseCache` map
/// (initially `Expiry="0"`, `Auth=""`) — gotohp core/api.go.
struct BearerCache {
    auth: String,
    expiry: String,
}

impl Default for BearerCache {
    fn default() -> Self {
        BearerCache {
            auth: String::new(),
            expiry: "0".to_string(),
        }
    }
}

/// The Photos protocol client — port of Go's `Api` struct.
/// One client per worker in the upload engine, as in Go; the bearer is
/// cached behind a mutex so sharing one client is also safe.
pub struct PhotosClient {
    /// Parsed credential pairs (query-style), replayed near-verbatim by
    /// the bearer exchange, exactly like Go re-parses `authData`.
    params: Vec<(String, String)>,
    language: String,
    user_agent: String,
    profile: DeviceProfile,
    agent: ureq::Agent,
    bearer: Mutex<BearerCache>,
}

impl PhotosClient {
    /// Port of gotohp `newAPIFromCredential` + the Saver/UseQuota half of
    /// `NewApi` (the credential here is already selected by the caller —
    /// CONTRACT.md freezes this signature).
    pub fn new(cred: Credential, opts: ApiOptions) -> Result<Self> {
        let credential_string = cred.to_string();
        let params = parse_query(&credential_string).map_err(|e| {
            Error::Auth(format!("failed to parse credentials: {e}"))
        })?;
        let language = get_param(&params, "lang").to_string();
        let profile = DeviceProfile::select(opts.saver, opts.use_quota);
        // gotohp core/api.go: the UA is formatted from the model at
        // construction time, which is always the default Pixel XL (Saver /
        // UseQuota are applied to the Api only afterwards in NewApi, and
        // the model mutation in CommitUpload happens later still). The
        // body device info follows `profile`; the UA does not.
        let user_agent = format!(
            "com.google.android.apps.photos/{} (Linux; U; Android 9; {}; {}; Build/{}; Cronet/127.0.6510.5) (gzip)",
            CLIENT_VERSION_CODE, language, MODEL_DEFAULT, ANDROID_BUILD_ID
        );
        let agent = build_agent(&opts.proxy)?;
        Ok(PhotosClient {
            params,
            language,
            user_agent,
            profile,
            agent,
            bearer: Mutex::new(BearerCache::default()),
        })
    }

    /// The device profile this client was constructed with.
    pub fn profile(&self) -> &DeviceProfile {
        &self.profile
    }

    /// Port of Go's `buildLivePhotoCommitPolicy(api)`
    /// (core/create_media_items.go): storage policy from the profile,
    /// upload quality always 1, device info from the (possibly
    /// Saver/UseQuota-adjusted) profile.
    pub fn live_photo_commit_policy(&self) -> LivePhotoCommitPolicy {
        LivePhotoCommitPolicy {
            storage_policy: self.profile.storage_policy,
            upload_quality: LIVE_PHOTO_UPLOAD_QUALITY,
            upload_device_info: self.profile.device_info(),
        }
    }

    // ------------------------------------------------------------------
    // Bearer exchange — gotohp core/api.go BearerToken + getAuthToken.
    // ------------------------------------------------------------------

    /// Port of Go's `BearerToken`: return the cached bearer while its
    /// expiry is in the future; otherwise perform ONE exchange. There is
    /// deliberately no 401 handling (a dead master token means manual
    /// re-capture), exactly as in Go.
    pub fn bearer_token(&self) -> Result<String> {
        {
            let cache = self.bearer.lock().unwrap();
            let expiry: i64 = cache
                .expiry
                .parse()
                .map_err(|e| Error::Auth(format!("invalid expiry time: {e}")))?;
            if expiry > now_unix() {
                if !cache.auth.is_empty() {
                    return Ok(cache.auth.clone());
                }
                // gotohp: a fresh-but-empty cache is an error, not a
                // reason to re-exchange.
                return Err(Error::Auth(
                    "auth response does not contain bearer token".to_string(),
                ));
            }
        }
        let fresh = self.fetch_bearer()?;
        let auth = fresh.auth.clone();
        *self.bearer.lock().unwrap() = fresh;
        Ok(auth)
    }

    /// Port of Go's `getAuthToken`. Replays the stored credential
    /// near-verbatim, with Go's exact edits: `app`/`callerPkg` forced to
    /// the Photos package, `it_caveat_types` and `assertion_jwt` deleted,
    /// `token_binding_alias` consumed (v1: unsupported, see below).
    fn fetch_bearer(&self) -> Result<BearerCache> {
        if !get_param(&self.params, "token_binding_alias").is_empty() {
            // gotohp prepares a token-binding session from the alias and
            // sets a fresh assertion_jwt. Token binding is out of scope
            // for v1 (CONTRACT decision 4); fail loudly instead of
            // sending a request Google would answer with an encrypted
            // bearer we cannot decrypt.
            return Err(Error::Auth(
                "token binding is not supported in this version; this credential requires it"
                    .to_string(),
            ));
        }

        let mut form: Vec<(String, String)> = self.params.clone();
        form_set(&mut form, "app", "com.google.android.apps.photos");
        form_set(&mut form, "callerPkg", "com.google.android.apps.photos");
        form_del(&mut form, "it_caveat_types");
        form_del(&mut form, "assertion_jwt");
        form_del(&mut form, "token_binding_alias");

        let android_id = get_param(&form, "androidId").to_string();
        let body = form_encode(&form).into_bytes();

        // Headers — gotohp core/api.go getAuthToken. The GoogleAuth UA
        // hardcodes the Pixel XL build in Go, regardless of profile.
        let headers: Vec<(String, String)> = vec![
            ("Accept-Encoding".into(), "gzip".into()),
            ("app".into(), "com.google.android.apps.photos".into()),
            ("Connection".into(), "Keep-Alive".into()),
            (
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
            ("device".into(), android_id),
            (
                "User-Agent".into(),
                format!("GoogleAuth/1.4 (Pixel XL {}); gzip", ANDROID_BUILD_ID),
            ),
        ];

        let resp = self.post_bytes(ANDROID_AUTH_ENDPOINT, body, &headers)?;
        ensure_success(resp.status, &resp.body)?;

        // Key=Value line response — same parser as gotohp
        // core/googleauth.go parseGoogleAuthResponse (a local copy is
        // used so this module does not depend on the auth module's
        // exact export name; behaviour is identical).
        let parsed = parse_auth_response(&resp.body);
        // gotohp core/tokenbinding.go decryptTokenEncryptedResponse with
        // a nil session: an encrypted answer we cannot decrypt is fatal.
        if parsed_get(&parsed, "TokenEncrypted") == "1" {
            return Err(Error::Auth(
                "auth response returned TokenEncrypted=1 but credential has no token_binding_alias"
                    .to_string(),
            ));
        }
        let auth = parsed_get(&parsed, "Auth").to_string();
        if auth.is_empty() {
            return Err(Error::Auth(
                "auth response missing Auth token".to_string(),
            ));
        }
        let expiry = parsed_get(&parsed, "Expiry").to_string();
        if expiry.is_empty() {
            return Err(Error::Auth(
                "auth response missing Expiry".to_string(),
            ));
        }
        Ok(BearerCache { auth, expiry })
    }

    // ------------------------------------------------------------------
    // HashCheck — gotohp core/api.go FindRemoteMediaByHash.
    // ------------------------------------------------------------------

    /// Check the library for an existing file with this SHA-1.
    /// Returns the media key when present (`Some`), `None` when the
    /// library has no match (Go returns `""`).
    pub fn find_remote_media_by_hash(&self, sha1: &[u8; 20]) -> Result<Option<String>> {
        // HashCheck::new builds the exact Go nesting, including the
        // empty field-2 message that is part of the captured request
        // shape: it serializes as a zero-length message field rather
        // than being omitted.
        let msg = HashCheck::new(sha1);
        let mut body = Vec::new();
        msg.encode(&mut body);

        let bearer = self.bearer_token()?;
        let resp = self.post_bytes(HASH_CHECK_ENDPOINT, body, &self.protobuf_headers(&bearer))?;
        ensure_success(resp.status, &resp.body)?;

        let decoded = RemoteMatches::decode(&resp.body).map_err(|e| {
            Error::Protocol(format!("failed to unmarshal protobuf: {e}"))
        })?;
        // RemoteMatches::media_key walks field1.field2.field2.media_key
        // (gotohp generated/utils.go GetMediaKey) and yields None when
        // any level is absent or the key is empty.
        Ok(decoded.media_key().map(|k| k.to_string()))
    }

    // ------------------------------------------------------------------
    // Scotty session + upload — gotohp core/api.go GetUploadToken,
    // UploadFileWithProgress, doUploadRequest.
    // ------------------------------------------------------------------

    /// Obtain a file upload session from the Scotty interactive
    /// endpoint. The header `X-Goog-Hash` carries the base64 SHA-1 (Go's
    /// callers pass `base64.StdEncoding` of the raw hash; CONTRACT.md
    /// freezes this port's input as raw bytes, so the encoding happens
    /// here). The session id is read ONLY from the
    /// `X-GUploader-UploadID` response header.
    pub fn get_upload_token(&self, sha1: &[u8; 20], size: u64) -> Result<UploadSession> {
        // gotohp generated.GetUploadToken{F1:2, F2:2, F3:1, F4:3,
        // FileSizeBytes} (.proto/GetUploadToken.proto, fields 1-4 + 7).
        let msg = GetUploadToken {
            f1: 2,
            f2: 2,
            f3: 1,
            f4: 3,
            file_size_bytes: size as i64,
        };
        let mut body = Vec::new();
        msg.encode(&mut body);

        let bearer = self.bearer_token()?;
        let mut headers = self.protobuf_headers(&bearer);
        headers.push((
            "X-Goog-Hash".into(),
            format!(
                "sha1={}",
                base64::engine::general_purpose::STANDARD.encode(sha1)
            ),
        ));
        headers.push(("X-Upload-Content-Length".into(), size.to_string()));

        let resp = self.post_bytes(SCOTTY_INTERACTIVE_ENDPOINT, body, &headers)?;
        ensure_success(resp.status, &resp.body)?;

        match resp.upload_id {
            Some(id) if !id.is_empty() => Ok(UploadSession { upload_id: id }),
            _ => Err(Error::Protocol(
                "response missing X-GUploader-UploadID header".to_string(),
            )),
        }
    }

    /// Upload one file into an opened session — port of Go's
    /// `UploadFileWithProgress`. The file is sent as a single chunked
    /// PUT (unknown content length, as Go's `ContentLength = -1`);
    /// on failure the WHOLE file is re-streamed, up to
    /// `MAX_RETRIES` retries, because the protocol has no resume.
    /// Every failure is retried (Go does not distinguish status classes
    /// in this loop); cancellation is checked before each attempt, during
    /// the backoff sleep, and after each failure.
    ///
    /// The `progress` callback receives `(bytes_uploaded, bytes_total)`
    /// per CONTRACT.md; Go's third argument (attempt) is not in the
    /// frozen signature, but `progress(0, total)` is still emitted at the
    /// start of every attempt so consumers can detect a restart.
    pub fn upload_file(
        &self,
        session: &UploadSession,
        path: &Path,
        progress: &(dyn Fn(u64, u64) + Sync),
        cancel: &CancellationToken,
    ) -> Result<ScottyToken> {
        // gotohp: stat first (needed for progress totals).
        let total = std::fs::metadata(path)
            .map_err(|e| Error::Io(e))?
            .len();
        let url = format!(
            "{}?upload_id={}",
            SCOTTY_INTERACTIVE_ENDPOINT, session.upload_id
        );

        let mut last_err: Option<Error> = None;
        for attempt in 0..=MAX_RETRIES {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if attempt > 0 {
                sleep_cancellable(calculate_backoff(attempt - 1), cancel)?;
            }
            // Signal start of this attempt (resets progress on retry).
            progress(0, total);
            match self.upload_attempt(&url, path, total, progress) {
                Ok(token) => return Ok(token),
                Err(e) => {
                    if cancel.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(Error::Http(format!(
            "upload failed after {} attempts: {}",
            MAX_RETRIES + 1,
            last_err.unwrap()
        )))
    }

    /// One upload attempt — port of Go's `doUploadRequest`. The file is
    /// opened fresh for each attempt (the key to not loading it into
    /// memory, per the Go comment).
    fn upload_attempt(
        &self,
        url: &str,
        path: &Path,
        total: u64,
        progress: &(dyn Fn(u64, u64) + Sync),
    ) -> Result<ScottyToken> {
        let file = File::open(path)?;
        let mut reader = CountingReader::new(file, total, progress);

        let bearer = self.bearer_token()?;
        let mut resp = self
            .agent
            .put(url)
            .header("Accept-Encoding", "gzip")
            .header("Accept-Language", self.language.as_str())
            .header("User-Agent", self.user_agent.as_str())
            .header("Authorization", format!("Bearer {bearer}"))
            // No Content-Length: ureq streams the reader with chunked
            // transfer encoding, matching Go's ContentLength = -1.
            // `SendBody::from_reader` is ureq 3's chunked-body
            // constructor for arbitrary `Read + Send + Sync` readers
            // (a bare reader does not implement `AsSendBody`).
            .send(ureq::SendBody::from_reader(&mut reader))
            .map_err(|e| Error::Http(format!("request failed: {e}")))?;

        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .read_to_vec()
            .map_err(|e| Error::Http(format!("failed to read response body: {e}")))?;
        ensure_success(status, &body)?;

        ScottyToken::parse(&body).map_err(|e| {
            Error::Protocol(format!("invalid upload finalize response: {e}"))
        })
    }

    // ------------------------------------------------------------------
    // Commit — gotohp core/api.go CommitUpload, CommitLivePhoto,
    // ReconcileLivePhoto, commitSerialized, doCommitRequest.
    // ------------------------------------------------------------------

    /// Commit a single-file upload — port of Go's `CommitUpload`.
    /// The Scotty token is converted through the legacy `CommitToken`
    /// decode (field 1 varint + field 2 opaque bytes), as in Go's
    /// `legacyCommitToken`; Live Photos bypass this and embed the raw
    /// token (see `commit_live_photo`).
    pub fn commit_upload(&self, token: &ScottyToken, file: &FileMeta) -> Result<String> {
        // gotohp core/scotty_token.go legacyCommitToken: re-validate at
        // the wire level, then decode as the legacy message.
        let raw = token.raw();
        ScottyToken::parse(raw)
            .map_err(|e| Error::Protocol(format!("invalid finalize token: {e}")))?;
        let legacy = CommitToken::decode(raw)
            .map_err(|e| Error::Protocol(format!("decode legacy commit token: {e}")))?;

        // gotohp: a zero upload timestamp means "now".
        let taken = if file.taken_unix_secs == 0 {
            now_unix()
        } else {
            file.taken_unix_secs
        };

        let msg = CommitUpload {
            field1: Some(CommitUploadField1 {
                // CommitUploadField1's token field IS the legacy
                // CommitToken message itself (protocol.rs), so the
                // decoded token moves in whole.
                field1: Some(legacy),
                file_name: file.name.clone(),
                sha1_hash: file.sha1.to_vec(),
                field4: Some(CommitUploadField4 {
                    file_last_modified_timestamp: taken,
                    field2: COMMIT_UNKNOWN_INT,
                }),
                quality: self.profile.commit_quality,
                field8: None,
                field10: 1,
                field17: 0,
            }),
            field2: Some(DeviceInfo {
                model: self.profile.model.to_string(),
                make: self.profile.make.to_string(),
                android_api_version: self.profile.android_api_version,
            }),
            field3: COMMIT_FIELD3.to_vec(),
        };
        let mut body = Vec::new();
        msg.encode(&mut body);
        self.commit_serialized(&body)
    }

    /// Commit a Live Photo (still + video in one blueprint) — port of
    /// Go's `CommitLivePhoto`. Build the request input with
    /// `live_photo_commit_policy()` for the policy fields.
    pub fn commit_live_photo(&self, input: LivePhotoCreateRequest) -> Result<String> {
        let body = build_live_photo_create_media_items_request(&input)?;
        self.commit_serialized(&body)
    }

    /// Reconcile an already-present still into a Live Photo by uploading
    /// only the video — port of Go's `ReconcileLivePhoto`.
    pub fn reconcile_live_photo(&self, input: LivePhotoReconcileRequest) -> Result<String> {
        let body = build_live_photo_reconcile_media_items_request(&input)?;
        self.commit_serialized(&body)
    }

    /// Port of Go's `commitSerialized`: the shared commit retry loop.
    /// Only retryable failures (network errors, bearer failures, and
    /// 5xx/429 statuses — Go's ShouldRetry via doCommitRequest) are
    /// retried, with backoff; everything else returns immediately.
    fn commit_serialized(&self, body: &[u8]) -> Result<String> {
        let mut last_err: Option<Error> = None;
        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                std::thread::sleep(calculate_backoff(attempt - 1));
            }
            match self.do_commit(body) {
                Ok(media_key) => return Ok(media_key),
                Err((err, retryable)) => {
                    if !retryable {
                        return Err(err);
                    }
                    last_err = Some(err);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            Error::Other(format!(
                "commit failed after {} attempts",
                MAX_RETRIES + 1
            ))
        }))
    }

    /// One commit attempt — port of Go's `doCommitRequest`.
    /// Returns `(error, retryable)` exactly as Go does.
    fn do_commit(&self, body: &[u8]) -> std::result::Result<String, (Error, bool)> {
        // gotohp: a bearer failure is retryable.
        let bearer = self
            .bearer_token()
            .map_err(|e| (wrap_bearer_err(e), true))?;

        let mut headers = self.protobuf_headers(&bearer);
        push_ext_headers(&mut headers);

        let resp = self
            .post_bytes(CREATE_MEDIA_ITEMS_ENDPOINT, body.to_vec(), &headers)
            .map_err(|e| (e, true))?;

        if !(200..300).contains(&resp.status) {
            return Err((
                Error::Http(format!(
                    "request failed with status {}: {}",
                    resp.status,
                    String::from_utf8_lossy(&resp.body)
                )),
                should_retry_status(resp.status),
            ));
        }

        // HTTP success may mean the media item already exists even when
        // the minimal response schema cannot validate it. Do not retry
        // and risk a duplicate commit. (Comment carried from Go.)
        match parse_create_media_items_response(&resp.body) {
            Ok(key) => Ok(key),
            Err(e) => Err((e, false)),
        }
    }

    // ------------------------------------------------------------------
    // Albums — gotohp core/api.go CreateAlbum + AddMediaToAlbum.
    // (Batching/retry orchestration lives in upload.rs, port of
    // core/album.go, per CONTRACT.md.)
    // ------------------------------------------------------------------

    /// Create a new album — port of Go's `CreateAlbum`, with the frozen
    /// CONTRACT.md signature (name only): the album is created empty and
    /// media is attached with `add_media_to_album`. Go creates the album
    /// with a first batch of media keys in the same request; the field
    /// is present in the message with zero entries here. This is the
    /// one deliberate wire-level divergence from Go in this module, and
    /// it is dictated by the frozen contract.
    pub fn create_album(&self, name: &str) -> Result<String> {
        let msg = CreateAlbum {
            album_name: name.to_string(),
            timestamp: now_unix(),
            field3: 1,
            media_keys: Vec::new(),
            field6: Some(CreateAlbumField6 {}),
            field7: Some(CreateAlbumField7 { field1: 3 }),
            device_info: Some(DeviceInfo {
                model: self.profile.model.to_string(),
                make: self.profile.make.to_string(),
                android_api_version: self.profile.android_api_version,
            }),
        };
        let mut body = Vec::new();
        msg.encode(&mut body);

        let bearer = self.bearer_token()?;
        let mut headers = self.protobuf_headers(&bearer);
        push_ext_headers(&mut headers);
        let resp = self.post_bytes(CREATE_ALBUM_ENDPOINT, body, &headers)?;
        if resp.status == 404 {
            return Err(Error::AlbumNotFound(name.to_string()));
        }
        ensure_success(resp.status, &resp.body)?;

        let decoded = CreateAlbumResponse::decode(&resp.body).map_err(|e| {
            Error::Protocol(format!("failed to unmarshal protobuf: {e}"))
        })?;
        // gotohp: both a missing field 1 and an empty key are failures.
        match decoded
            .field1
            .map(|f| f.album_media_key)
            .filter(|k| !k.is_empty())
        {
            Some(key) => Ok(key),
            None => Err(Error::Protocol(
                "create album failed: no album media key returned".to_string(),
            )),
        }
    }

    /// Add media items to an existing album — port of Go's
    /// `AddMediaToAlbum`. A 404 from the server (unknown album key) is
    /// mapped to `Error::AlbumNotFound`, the distinction the UI relies
    /// on (CONTRACT.md; the Go UI special-cases the same case).
    pub fn add_media_to_album(
        &self,
        album_key: &str,
        media_keys: &[String],
    ) -> Result<()> {
        let msg = AddMediaToAlbum {
            media_keys: media_keys.to_vec(),
            album_media_key: album_key.to_string(),
            field5: Some(AddMediaToAlbumField5 { field1: 2 }),
            device_info: Some(DeviceInfo {
                model: self.profile.model.to_string(),
                make: self.profile.make.to_string(),
                android_api_version: self.profile.android_api_version,
            }),
            timestamp: now_unix(),
        };
        let mut body = Vec::new();
        msg.encode(&mut body);

        let bearer = self.bearer_token()?;
        let mut headers = self.protobuf_headers(&bearer);
        push_ext_headers(&mut headers);
        let resp = self.post_bytes(ADD_MEDIA_TO_ALBUM_ENDPOINT, body, &headers)?;
        if resp.status == 404 {
            return Err(Error::AlbumNotFound(album_key.to_string()));
        }
        ensure_success(resp.status, &resp.body)?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Shared request plumbing.
    // ------------------------------------------------------------------

    /// Standard protobuf-call headers — gotohp core/api.go (identical in
    /// GetUploadToken / FindRemoteMediaByHash; commit and album calls add
    /// the x-goog-ext pair via `push_ext_headers`).
    fn protobuf_headers(&self, bearer: &str) -> Vec<(String, String)> {
        vec![
            ("Accept-Encoding".into(), "gzip".into()),
            ("Accept-Language".into(), self.language.clone()),
            ("Content-Type".into(), "application/x-protobuf".into()),
            ("User-Agent".into(), self.user_agent.clone()),
            ("Authorization".into(), format!("Bearer {bearer}")),
        ]
    }

    /// POST a byte body and return the raw response (status + full body +
    /// the Scotty session header). Non-2xx is NOT an error here; callers
    /// apply `ensure_success` or their own status semantics, as Go does.
    /// ureq is configured with `http_status_as_error(false)` to make this
    /// possible, and gzip decompression is handled by ureq (Go sets
    /// `Accept-Encoding: gzip` and decompresses manually in
    /// ReadResponseBody; the wire result is the same).
    fn post_bytes(
        &self,
        url: &str,
        body: Vec<u8>,
        headers: &[(String, String)],
    ) -> Result<RawResponse> {
        let mut req = self.agent.post(url);
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let mut resp = req
            .send(body)
            .map_err(|e| Error::Http(format!("request failed: {e}")))?;
        let status = resp.status().as_u16();
        let upload_id = resp
            .headers()
            .get("X-GUploader-UploadID")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let body = resp
            .body_mut()
            .read_to_vec()
            .map_err(|e| Error::Http(format!("failed to read response body: {e}")))?;
        Ok(RawResponse {
            status,
            body,
            upload_id,
        })
    }
}

/// A raw HTTP response as used by this module.
struct RawResponse {
    status: u16,
    body: Vec<u8>,
    /// `X-GUploader-UploadID` response header, when present.
    upload_id: Option<String>,
}

/// Build the shared ureq agent. TLS verification is left at ureq's
/// verified default and is never disabled (CONTRACT.md decision 2).
/// Redirects are never followed: several of this client's requests carry
/// bearer/master-token material (compare gotohp core/googleauth.go
/// rejectGoogleAuthRedirect).
fn build_agent(proxy: &str) -> Result<ureq::Agent> {
    let mut builder = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0);
    if !proxy.trim().is_empty() {
        let parsed = ureq::Proxy::new(proxy)
            .map_err(|e| Error::Config(format!("invalid proxy URL: {e}")))?;
        builder = builder.proxy(Some(parsed));
    }
    Ok(ureq::Agent::new_with_config(builder.build()))
}

/// gotohp's non-2xx error shape, shared by every call in api.go:
/// `request failed with status %d: %s`. The album layer in Go retries by
/// substring-matching "status 5"/"status 429" in this text; the format is
/// preserved so equivalent matching keeps working.
fn ensure_success(status: u16, body: &[u8]) -> Result<()> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(Error::Http(format!(
            "request failed with status {}: {}",
            status,
            String::from_utf8_lossy(body)
        )))
    }
}

/// The x-goog-ext-*-bin magic header pair carried by commit and album
/// calls — gotohp core/api.go (byte-exact values, see the consts).
fn push_ext_headers(headers: &mut Vec<(String, String)>) {
    headers.push((
        "x-goog-ext-173412678-bin".into(),
        X_GOOG_EXT_173412678_BIN.into(),
    ));
    headers.push((
        "x-goog-ext-174067345-bin".into(),
        X_GOOG_EXT_174067345_BIN.into(),
    ));
}

fn wrap_bearer_err(e: Error) -> Error {
    match e {
        Error::Auth(m) => Error::Auth(format!("failed to get bearer token: {m}")),
        other => other,
    }
}

/// Decode only the verified media-key path — port of Go's
/// parseCreateMediaItemsResponse (core/api.go). Any 2xx body that does
/// not yield a media key becomes `Error::CommitAmbiguous`: the media may
/// exist server-side and the commit must never be retried (Go returns a
/// plain non-retryable error for both the unmarshal failure and the
/// empty-key case; CONTRACT.md names the variant).
fn parse_create_media_items_response(body: &[u8]) -> Result<String> {
    let decoded = CreateMediaItemsResponse::decode(body).map_err(|e| {
        Error::CommitAmbiguous(format!("failed to parse accepted response: {e}"))
    })?;
    for item in &decoded.item {
        if let Some(result) = &item.result_item {
            if !result.media_key.is_empty() {
                return Ok(result.media_key.clone());
            }
        }
    }
    Err(Error::CommitAmbiguous(
        "upload rejected by API: media key is empty or missing".to_string(),
    ))
}

// (The media-key path of a HashCheck response —
// `field1.field2.field2.media_key` — is `RemoteMatches::media_key` in
// protocol.rs, port of the hand-written getter in gotohp
// generated/utils.go.)

// ---------------------------------------------------------------------------
// Counting reader — port of gotohp core/progress_reader.go.
// ---------------------------------------------------------------------------

/// Wraps the file being uploaded, counting bytes as they pass through
/// and invoking the progress callback at most every 100 ms (10 updates
/// per second), plus a final emission at EOF — gotohp
/// core/progress_reader.go ProgressReader.
pub struct CountingReader<'a> {
    inner: File,
    total: u64,
    read: u64,
    last_emit: Instant,
    final_emitted: bool,
    sink: ProgressSink<'a>,
}

/// The progress callback is invoked only from the thread performing the
/// (synchronous) upload, sequentially, never concurrently; ureq reads
/// the body on the calling thread. Requiring the callback itself to be
/// `Sync` makes the shared reference `Send + Sync`, so `CountingReader`
/// satisfies the `Read + Send + Sync` bound of ureq's
/// `SendBody::from_reader` with no unsafe impls.
struct ProgressSink<'a>(&'a (dyn Fn(u64, u64) + Sync));

impl<'a> CountingReader<'a> {
    pub fn new(inner: File, total: u64, on_progress: &'a (dyn Fn(u64, u64) + Sync)) -> Self {
        CountingReader {
            inner,
            total,
            read: 0,
            // Go's zero-value lastEmit makes the first read emit
            // immediately; backdating by the interval reproduces that.
            last_emit: Instant::now()
                .checked_sub(PROGRESS_EMIT_INTERVAL)
                .unwrap_or_else(Instant::now),
            final_emitted: false,
            sink: ProgressSink(on_progress),
        }
    }

    /// Total bytes passed through so far (Go: BytesRead).
    pub fn bytes_read(&self) -> u64 {
        self.read
    }

    fn emit(&self) {
        (self.sink.0)(self.read, self.total);
    }
}

/// gotohp core/progress_reader.go: throttle interval.
pub const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(100);

impl Read for CountingReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.read += n as u64;
            if self.last_emit.elapsed() >= PROGRESS_EMIT_INTERVAL || self.read >= self.total {
                self.last_emit = Instant::now();
                self.emit();
            }
        } else if !self.final_emitted {
            // EOF: Go emits once more when the read returns io.EOF.
            self.final_emitted = true;
            self.emit();
        }
        Ok(n)
    }
}

/// Sleep for `delay`, waking early with `Error::Cancelled` if the token
/// is cancelled (Go's select on ctx.Done() during upload backoff).
fn sleep_cancellable(delay: Duration, cancel: &CancellationToken) -> Result<()> {
    let start = Instant::now();
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let elapsed = start.elapsed();
        if elapsed >= delay {
            return Ok(());
        }
        std::thread::sleep(std::cmp::min(delay - elapsed, Duration::from_millis(25)));
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Form / query helpers — Go net/url semantics (ParseQuery, Values.Encode,
// QueryEscape), needed because the credential string and the auth
// response are both key=value formats. Kept local to this module (the
// credential module owns the credential string itself; this is the
// bearer exchange's replay machinery).
// ---------------------------------------------------------------------------

/// Parse a query-style string — Go's `url.ParseQuery`: pairs split on
/// `&`, key/value on the first `=`, `+` means space, `%XX` decoding.
fn parse_query(s: &str) -> std::result::Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for pair in s.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        out.push((query_unescape(k)?, query_unescape(v)?));
    }
    Ok(out)
}

fn get_param<'a>(params: &'a [(String, String)], key: &str) -> &'a str {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}

/// Go `url.Values.Set`: replace all values for `key` with a single one.
fn form_set(form: &mut Vec<(String, String)>, key: &str, value: &str) {
    form.retain(|(k, _)| k != key);
    form.push((key.to_string(), value.to_string()));
}

/// Go `url.Values.Del`.
fn form_del(form: &mut Vec<(String, String)>, key: &str) {
    form.retain(|(k, _)| k != key);
}

/// Go `url.Values.Encode`: keys sorted ascending (stable within a key),
/// each side QueryEscape'd, pairs joined with `&`.
fn form_encode(form: &[(String, String)]) -> String {
    let mut pairs: Vec<&(String, String)> = form.iter().collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", query_escape(k), query_escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Go `url.QueryEscape`: unreserved bytes pass through, space becomes
/// `+`, everything else `%XX` (uppercase hex, UTF-8 bytes).
fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Go `url.QueryUnescape`.
fn query_unescape(s: &str) -> std::result::Result<String, String> {
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
                    return Err(format!("invalid URL escape in {s:?}"));
                }
                let hi = hex_val(bytes[i + 1])?;
                let lo = hex_val(bytes[i + 2])?;
                out.push(hi * 16 + lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|e| format!("invalid UTF-8 in query value: {e}"))
}

fn hex_val(b: u8) -> std::result::Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(format!("invalid hex digit in URL escape: {}", b as char)),
    }
}

/// Parse the key=value line format of Google auth responses —
/// gotohp core/googleauth.go parseGoogleAuthResponse (also the inline
/// parser in core/api.go getAuthToken; they are identical). Used instead
/// of `crate::auth`'s export so this module carries no assumption about
/// that export's name; if the auth module exposes the same function,
/// integration may deduplicate.
fn parse_auth_response(body: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(body);
    let mut out = Vec::new();
    for line in text.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.push((k.to_string(), v.to_string()));
        }
    }
    out
}

fn parsed_get<'a>(parsed: &'a [(String, String)], key: &str) -> &'a str {
    get_param(parsed, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_default() {
        let p = DeviceProfile::select(false, false);
        assert_eq!(p.model, "Pixel XL");
        assert_eq!(p.storage_policy, 3);
        assert_eq!(p.commit_quality, 3);
    }

    #[test]
    fn profile_saver() {
        let p = DeviceProfile::select(true, false);
        assert_eq!(p.model, "Pixel 2");
        assert_eq!(p.storage_policy, 1);
        assert_eq!(p.commit_quality, 1);
    }

    #[test]
    fn profile_use_quota_overrides_model_only() {
        let p = DeviceProfile::select(false, true);
        assert_eq!(p.model, "Pixel 8");
        assert_eq!(p.storage_policy, 3);
        assert_eq!(p.commit_quality, 3);
    }

    #[test]
    fn profile_saver_and_quota_matches_go_mutation_order() {
        // Go: Saver sets model/policy/quality, then UseQuota overwrites
        // only the model.
        let p = DeviceProfile::select(true, true);
        assert_eq!(p.model, "Pixel 8");
        assert_eq!(p.storage_policy, 1);
        assert_eq!(p.commit_quality, 1);
    }

    #[test]
    fn query_round_trip() {
        let form = vec![
            ("Email".to_string(), "a b@example.com".to_string()),
            ("Token".to_string(), "aas_et/x+y=".to_string()),
            ("lang".to_string(), "en_US".to_string()),
        ];
        let encoded = form_encode(&form);
        // Keys sorted ascending like Go's url.Values.Encode.
        assert!(encoded.starts_with("Email="));
        let parsed = parse_query(&encoded).unwrap();
        assert_eq!(get_param(&parsed, "Email"), "a b@example.com");
        assert_eq!(get_param(&parsed, "Token"), "aas_et/x+y=");
        assert_eq!(get_param(&parsed, "lang"), "en_US");
    }

    #[test]
    fn form_set_and_del_match_go_values_semantics() {
        let mut form = vec![
            ("app".to_string(), "old".to_string()),
            ("assertion_jwt".to_string(), "x".to_string()),
            ("app".to_string(), "older".to_string()),
        ];
        form_set(&mut form, "app", "com.google.android.apps.photos");
        form_del(&mut form, "assertion_jwt");
        let apps: Vec<&str> = form
            .iter()
            .filter(|(k, _)| k == "app")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(apps, vec!["com.google.android.apps.photos"]);
        assert_eq!(get_param(&form, "assertion_jwt"), "");
    }

    #[test]
    fn auth_response_parsing_matches_go() {
        let parsed = parse_auth_response(b"Auth=tok123\nExpiry=1893456000\n\nIgnoredLine\n");
        assert_eq!(parsed_get(&parsed, "Auth"), "tok123");
        assert_eq!(parsed_get(&parsed, "Expiry"), "1893456000");
    }

    #[test]
    fn backoff_bounds() {
        // Base sequence 1s, 2s, 4s, 8s capped at 30s, + up to 10% jitter.
        for (attempt, base_ms) in [(0u32, 1000u64), (1, 2000), (2, 4000), (3, 8000), (9, 30000)] {
            let d = calculate_backoff(attempt).as_millis() as u64;
            assert!(d >= base_ms && d <= base_ms + base_ms / 10, "attempt {attempt}: {d}");
        }
    }

    #[test]
    fn retry_status_classes() {
        assert!(should_retry_status(500));
        assert!(should_retry_status(503));
        assert!(should_retry_status(429));
        assert!(!should_retry_status(400));
        assert!(!should_retry_status(404));
        assert!(!should_retry_status(200));
    }
}
