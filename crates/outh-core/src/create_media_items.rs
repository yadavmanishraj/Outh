//! Port of gotohp `core/create_media_items.go` (HEAD 97a5dc0).
//!
//! Builds the `CreateMediaItems` protobuf requests used to commit Live
//! Photos: the create form (still + video committed in ONE blueprint via
//! `live_photo_info`, field 24) and the reconcile form (a video-led
//! blueprint that locates an already-uploaded still by SHA-1 via
//! `reconcile_info`). Both are sent to the shared commit RPC in `api.rs`;
//! pairing changes the request body, not the RPC (gotohp core/api.go).
//!
//! Scotty finalize tokens are embedded as opaque raw bytes, byte-for-byte;
//! they are never decoded and re-encoded here (gotohp core/scotty_token.go).
//!
//! The message types come from `crate::protocol` (the `CreateMediaItems`
//! message family in gotohp `.proto/CreateMediaItems.proto`); this module
//! only assembles them from the validated request structs below.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;

use crate::protocol::{
    CreateMediaItemsRequest, LivePhotoInfo, MediaItemBlueprint, ReconcileInfo, ScottyToken,
    UploadDeviceInfo as ProtoUploadDeviceInfo, UploadTimestamp,
};
use crate::{Error, Result};

/// `RECONCILE_TYPE_PHODEO` (gotohp .proto/CreateMediaItems.proto,
/// generated.ReconcileType_RECONCILE_TYPE_PHODEO).
pub const RECONCILE_TYPE_PHODEO: i32 = 1;

/// This opaque result-item mask occupied top-level field 5 and was identical
/// in all 17 accepted official-client requests across the scoped captures.
/// Copied VERBATIM from gotohp core/create_media_items.go
/// (`livePhotoResultItemMaskBase64`); do not interpret or regenerate it.
pub const LIVE_PHOTO_RESULT_ITEM_MASK_BASE64: &str = "CnsKAggBEAEaAggBIgIIASoQCgIIARICCAEaAggBIgIIATICCAGCAQIIAYoBAggBmgECCAGiAQIIAaoBCggBKgIgATICCAHKAQIIAfIBBggBEgIIAfoBAggBggICCAGKAgQKAggBkgIGCAESAggBogICCAGqAgC6AgDCAgAQASpiCAESKggBEhIIARoGCAESAggBIgQIASIAKAEiBggBEgIQASoGCAESAggBMAE4ARoWCAESDAgBGgIIASICCAEoARoCCAEwASIKCAESBggBEgIIASoOCgQIATABEgYIARICCAE6AggBQgIIAUoKCAESAggBGgISAFoSEgIIARoCCAEiCAgBEgQIARACYgIIAXISEgIIARoCCAEiCAgBEgQIARACeggKAggBIgIIAYoBBAoCCAGSAQQIARABqgECCgA=";

/// A point in time with nanosecond precision, mirroring Go's `time.Time`
/// inputs to the builders (which convert via `t.Unix()` / `t.Nanosecond()`
/// into the `UploadTimestamp` protocol message).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MediaTimestamp {
    pub seconds: i64,
    pub nanoseconds: i64,
}

impl MediaTimestamp {
    pub fn from_unix(seconds: i64, nanoseconds: i64) -> Self {
        MediaTimestamp {
            seconds,
            nanoseconds,
        }
    }

    pub fn from_unix_secs(seconds: i64) -> Self {
        MediaTimestamp {
            seconds,
            nanoseconds: 0,
        }
    }

    pub fn from_system_time(t: SystemTime) -> Self {
        match t.duration_since(UNIX_EPOCH) {
            Ok(d) => MediaTimestamp {
                seconds: d.as_secs() as i64,
                nanoseconds: d.subsec_nanos() as i64,
            },
            Err(e) => {
                // Pre-epoch: mirror Go's Unix()/Nanosecond() decomposition
                // (negative seconds, non-negative nanoseconds).
                let d = e.duration();
                if d.subsec_nanos() == 0 {
                    MediaTimestamp {
                        seconds: -(d.as_secs() as i64),
                        nanoseconds: 0,
                    }
                } else {
                    MediaTimestamp {
                        seconds: -(d.as_secs() as i64) - 1,
                        nanoseconds: 1_000_000_000 - d.subsec_nanos() as i64,
                    }
                }
            }
        }
    }

    /// Mirrors Go's `time.Time.IsZero()` guard usage: the zero value is the
    /// only rejected timestamp.
    fn is_zero(&self) -> bool {
        self.seconds == 0 && self.nanoseconds == 0
    }

    fn to_proto(&self) -> UploadTimestamp {
        UploadTimestamp {
            seconds: self.seconds,
            nanoseconds: self.nanoseconds,
        }
    }
}

/// Core-level device identity carried in commit requests — port of Go's
/// `core.UploadDeviceInfo` (create_media_items.go). Distinct from the
/// protocol message of the same name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadDeviceInfo {
    pub model: String,
    pub make: String,
    pub android_api_version: i64,
}

impl UploadDeviceInfo {
    fn to_proto(&self) -> ProtoUploadDeviceInfo {
        ProtoUploadDeviceInfo {
            model: self.model.clone(),
            make: self.make.clone(),
            android_api_version: self.android_api_version,
        }
    }
}

/// Port of Go's `LivePhotoCreateRequest` (create_media_items.go).
#[derive(Clone, Debug)]
pub struct LivePhotoCreateRequest {
    pub photo_token: ScottyToken,
    pub video_token: ScottyToken,
    pub file_name: String,
    pub photo_sha1: Vec<u8>,
    pub video_sha1: Vec<u8>,
    pub created_at: MediaTimestamp,
    pub modified_at: MediaTimestamp,
    pub storage_policy: i64,
    pub upload_quality: i64,
    pub upload_device_info: UploadDeviceInfo,
}

/// Port of Go's `LivePhotoReconcileRequest` (create_media_items.go).
/// `file_name` is the VIDEO file name (the reconcile blueprint is
/// video-led; gotohp core/livephoto_upload.go passes `videoInfo.Name()`).
#[derive(Clone, Debug)]
pub struct LivePhotoReconcileRequest {
    pub video_token: ScottyToken,
    pub file_name: String,
    pub photo_sha1: Vec<u8>,
    pub video_sha1: Vec<u8>,
    pub created_at: MediaTimestamp,
    pub modified_at: MediaTimestamp,
    pub storage_policy: i64,
    pub upload_quality: i64,
    pub upload_device_info: UploadDeviceInfo,
}

/// Port of Go's `LivePhotoCommitPolicy` (create_media_items.go). Built by
/// `PhotosClient::live_photo_commit_policy` in api.rs, mirroring Go's
/// `buildLivePhotoCommitPolicy(api)`.
#[derive(Clone, Debug)]
pub struct LivePhotoCommitPolicy {
    pub storage_policy: i64,
    pub upload_quality: i64,
    pub upload_device_info: UploadDeviceInfo,
}

fn result_item_mask() -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(LIVE_PHOTO_RESULT_ITEM_MASK_BASE64)
        .map_err(|e| Error::Protocol(format!("decode result item mask: {e}")))
}

/// Validation shared by both builders, mirroring the guard sequence in
/// gotohp core/create_media_items.go. `kind` is "photo" or "video" and only
/// selects the wording of the filename/timestamp messages, as in Go.
fn validate_common(
    file_name: &str,
    photo_sha1: &[u8],
    video_sha1: &[u8],
    created_at: MediaTimestamp,
    modified_at: MediaTimestamp,
    storage_policy: i64,
    upload_quality: i64,
    device: &UploadDeviceInfo,
    kind: &str,
) -> Result<()> {
    if file_name.trim().is_empty() {
        return Err(Error::Protocol(format!("{kind} filename is required")));
    }
    if photo_sha1.len() != 20 || video_sha1.len() != 20 {
        return Err(Error::Protocol(
            "photo and video SHA-1 values must each contain 20 bytes".to_string(),
        ));
    }
    if created_at.is_zero() || modified_at.is_zero() {
        return Err(Error::Protocol(format!(
            "{kind} creation and modification times are required"
        )));
    }
    if storage_policy <= 0 || upload_quality <= 0 {
        return Err(Error::Protocol(
            "storage policy and upload quality must be positive".to_string(),
        ));
    }
    if device.model.is_empty() || device.make.is_empty() || device.android_api_version <= 0 {
        return Err(Error::Protocol(
            "complete Android upload device information is required".to_string(),
        ));
    }
    Ok(())
}

/// Port of Go's `BuildLivePhotoCreateMediaItemsRequest`
/// (core/create_media_items.go). Returns the serialized
/// `CreateMediaItemsRequest` protobuf bytes.
///
/// Build the official-client invariant shape while keeping Scotty tokens as
/// opaque bytes. Blueprint fields are upload_token=1, file_name=2,
/// source_sha1=3, create_time=5, mod_time=6, storage_policy=7,
/// upload_quality=10, and live_photo_info=24. LivePhotoInfo contains
/// video_upload_token=1 and video_source_sha1=2. Client capabilities
/// (top-level 3) and video_crc32c (24.3) are schema fields but were absent
/// from every accepted scoped request; passive captures do not prove the
/// server's absolute minimum. (Comment carried from the Go source.)
pub fn build_live_photo_create_media_items_request(
    input: &LivePhotoCreateRequest,
) -> Result<Vec<u8>> {
    // gotohp: both finalize tokens are re-validated at the wire level.
    ScottyToken::parse(input.photo_token.raw())
        .map_err(|e| Error::Protocol(format!("invalid photo finalize token: {e}")))?;
    ScottyToken::parse(input.video_token.raw())
        .map_err(|e| Error::Protocol(format!("invalid video finalize token: {e}")))?;
    validate_common(
        &input.file_name,
        &input.photo_sha1,
        &input.video_sha1,
        input.created_at,
        input.modified_at,
        input.storage_policy,
        input.upload_quality,
        &input.upload_device_info,
        "photo",
    )?;

    let request = CreateMediaItemsRequest {
        blueprint_array: vec![MediaItemBlueprint {
            upload_token: input.photo_token.raw().to_vec(),
            file_name: input.file_name.clone(),
            source_sha1: input.photo_sha1.clone(),
            filesystem_create_time: Some(input.created_at.to_proto()),
            filesystem_mod_time: Some(input.modified_at.to_proto()),
            storage_policy: input.storage_policy,
            reconcile_info: None,
            upload_quality: input.upload_quality,
            live_photo_info: Some(LivePhotoInfo {
                video_upload_token: input.video_token.raw().to_vec(),
                video_source_sha1: input.video_sha1.clone(),
            }),
        }],
        upload_device_info: Some(input.upload_device_info.to_proto()),
        result_item_mask: result_item_mask()?,
    };

    let mut out = Vec::new();
    request.encode(&mut out);
    Ok(out)
}

/// Port of Go's `BuildLivePhotoReconcileMediaItemsRequest`
/// (core/create_media_items.go). Returns the serialized
/// `CreateMediaItemsRequest` protobuf bytes.
///
/// The verified HEIC-first reconciliation shape is video-led. Google
/// accepts the MOV as the primary blueprint and locates the existing still
/// by SHA-1; live_photo_info and the recursive photo_upload_blueprint must
/// remain absent. (Comment carried from the Go source.)
pub fn build_live_photo_reconcile_media_items_request(
    input: &LivePhotoReconcileRequest,
) -> Result<Vec<u8>> {
    ScottyToken::parse(input.video_token.raw())
        .map_err(|e| Error::Protocol(format!("invalid video finalize token: {e}")))?;
    validate_common(
        &input.file_name,
        &input.photo_sha1,
        &input.video_sha1,
        input.created_at,
        input.modified_at,
        input.storage_policy,
        input.upload_quality,
        &input.upload_device_info,
        "video",
    )?;

    let request = CreateMediaItemsRequest {
        blueprint_array: vec![MediaItemBlueprint {
            upload_token: input.video_token.raw().to_vec(),
            file_name: input.file_name.clone(),
            source_sha1: input.video_sha1.clone(),
            filesystem_create_time: Some(input.created_at.to_proto()),
            filesystem_mod_time: Some(input.modified_at.to_proto()),
            storage_policy: input.storage_policy,
            reconcile_info: Some(ReconcileInfo {
                reconcile_type: RECONCILE_TYPE_PHODEO,
                source_sha1: input.photo_sha1.clone(),
                photo_upload_blueprint: None,
            }),
            upload_quality: input.upload_quality,
            live_photo_info: None,
        }],
        upload_device_info: Some(input.upload_device_info.to_proto()),
        result_item_mask: result_item_mask()?,
    };

    let mut out = Vec::new();
    request.encode(&mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build wire-valid Scotty finalize token bytes by hand:
    /// field 1 varint = 2, field 2 bytes = opaque payload
    /// (gotohp core/scotty_token.go validation shape).
    fn token_bytes(payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0x08, 0x02, 0x12, payload.len() as u8];
        v.extend_from_slice(payload);
        v
    }

    fn token(payload: &[u8]) -> ScottyToken {
        ScottyToken::parse(&token_bytes(payload)).unwrap()
    }

    fn device() -> UploadDeviceInfo {
        UploadDeviceInfo {
            model: "Pixel XL".to_string(),
            make: "Google".to_string(),
            android_api_version: 28,
        }
    }

    fn create_request() -> LivePhotoCreateRequest {
        LivePhotoCreateRequest {
            photo_token: token(b"photo"),
            video_token: token(b"video"),
            file_name: "IMG_0001.HEIC".to_string(),
            photo_sha1: vec![1u8; 20],
            video_sha1: vec![2u8; 20],
            created_at: MediaTimestamp::from_unix_secs(1_700_000_000),
            modified_at: MediaTimestamp::from_unix_secs(1_700_000_000),
            storage_policy: 3,
            upload_quality: 1,
            upload_device_info: device(),
        }
    }

    fn reconcile_request() -> LivePhotoReconcileRequest {
        LivePhotoReconcileRequest {
            video_token: token(b"video"),
            file_name: "IMG_0001.MOV".to_string(),
            photo_sha1: vec![1u8; 20],
            video_sha1: vec![2u8; 20],
            created_at: MediaTimestamp::from_unix_secs(1_700_000_000),
            modified_at: MediaTimestamp::from_unix_secs(1_700_000_000),
            storage_policy: 3,
            upload_quality: 1,
            upload_device_info: device(),
        }
    }

    #[test]
    fn mask_decodes_to_go_length() {
        // The base64 const is copied verbatim from gotohp
        // core/create_media_items.go; it decodes to 320 bytes.
        let mask = result_item_mask().unwrap();
        assert_eq!(mask.len(), 320);
    }

    #[test]
    fn create_request_encodes() {
        let bytes = build_live_photo_create_media_items_request(&create_request()).unwrap();
        assert!(!bytes.is_empty());
        // Top-level field 1 (blueprint_array, wire type 2) tag comes first.
        assert_eq!(bytes[0], 0x0A);
    }

    #[test]
    fn create_request_rejects_short_sha1() {
        let mut req = create_request();
        req.photo_sha1 = vec![1u8; 19];
        let err = build_live_photo_create_media_items_request(&req).unwrap_err();
        assert!(format!("{err}").contains("SHA-1"));
    }

    #[test]
    fn create_request_rejects_zero_times() {
        let mut req = create_request();
        req.created_at = MediaTimestamp::default();
        assert!(build_live_photo_create_media_items_request(&req).is_err());
    }

    #[test]
    fn create_request_rejects_nonpositive_policy() {
        let mut req = create_request();
        req.storage_policy = 0;
        assert!(build_live_photo_create_media_items_request(&req).is_err());
    }

    #[test]
    fn reconcile_request_encodes() {
        let bytes =
            build_live_photo_reconcile_media_items_request(&reconcile_request()).unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(bytes[0], 0x0A);
    }

    #[test]
    fn reconcile_request_rejects_empty_filename() {
        let mut req = reconcile_request();
        req.file_name = "  ".to_string();
        assert!(build_live_photo_reconcile_media_items_request(&req).is_err());
    }
}
