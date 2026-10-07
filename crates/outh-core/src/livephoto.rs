//! Live Photo pairing / classification — port of gotohp `core/livephoto.go`
//! (290 lines, HEAD 97a5dc0). The upload engine consumes this output the way
//! Go's `upload.go` consumes `ClassifyUploadWork`: the work items are the
//! unit of work for workers, and the warnings are reported as preflight
//! warnings before the run starts.
//!
//! ## Output contract (adaptation points — read before consuming)
//!
//! * [`WorkItem::LivePhoto`]'s `identifier` is the *pairing key*: the
//!   lower-cased Apple content identifier in metadata mode, and the empty
//!   string in filename-stem mode (Go blanks `ContentIdentifier` there too).
//! * Go's `PreflightWarning` carries a path list + a `Code`; the frozen
//!   core type in `types.rs` carries one `file_path` + `message`. Each Go
//!   warning becomes exactly one [`PreflightWarning`] here: `file_path` is
//!   the first path of Go's list, and the message is prefixed with Go's
//!   code (`"ambiguous-identifier: …"`); for multi-path warnings the full
//!   path list is appended to the message.
//! * [`classify`] is the fixed-signature entry point. It supplies Go's
//!   remaining option from the app default: `skip_incomplete = true`
//!   (gotohp `DefaultPreferences.SkipIncompleteLivePhotos`). Callers that
//!   need the user's actual preference must use
//!   [`classify_with_options`].
//! * Go's classification also takes a cancellation callback and returns
//!   empty output when cancelled. That knob is deliberately not in the
//!   frozen signature; [`classify_with_options`] mirrors Go's option set
//!   minus cancellation — the upload engine checks its
//!   `CancellationToken` around classification instead.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::livephoto_metadata::{content_identifier_still, read_video_metadata, MovInfo};
use crate::types::PreflightWarning;
use crate::Result;

/// One unit of upload work — port of Go's `UploadWorkItem`. Go models this
/// as a kind tag plus two optional pointers; the Rust enum makes the
/// "exactly one payload" invariant structural instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkItem {
    /// A file that uploads on its own (Go `UploadWorkSingle`).
    Single(PathBuf),
    /// A still + video pair that commits as one Live Photo
    /// (Go `UploadWorkLivePhoto` / `LivePhotoPair`).
    LivePhoto {
        still: PathBuf,
        video: PathBuf,
        /// Lower-cased Apple content identifier, or empty in
        /// filename-stem mode.
        identifier: String,
    },
}

impl WorkItem {
    /// All file paths in the item — port of Go's `uploadWorkPaths`:
    /// `[still, video]` for a Live Photo, `[path]` for a single.
    pub fn paths(&self) -> Vec<&Path> {
        match self {
            WorkItem::Single(path) => vec![path.as_path()],
            WorkItem::LivePhoto { still, video, .. } => {
                vec![still.as_path(), video.as_path()]
            }
        }
    }

    /// The path results/progress are reported against — port of Go's
    /// `uploadWorkPrimaryPath` (the still for a Live Photo).
    pub fn primary_path(&self) -> &Path {
        match self {
            WorkItem::Single(path) => path.as_path(),
            WorkItem::LivePhoto { still, .. } => still.as_path(),
        }
    }
}

/// Result of classification — Go returns `(items, warnings)` as two values.
/// (No `PartialEq`: `PreflightWarning` in types.rs doesn't derive it.)
#[derive(Debug, Clone, Default)]
pub struct ClassifiedWork {
    pub items: Vec<WorkItem>,
    pub warnings: Vec<PreflightWarning>,
}

/// Classification options — port of Go's
/// `LivePhotoClassificationOptions`, minus the cancellation callback
/// (see module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassifyOptions {
    /// Go `Enabled` (`PairLivePhotos` preference). When false, every
    /// input is a single and metadata is never read.
    pub pair_live_photos: bool,
    /// Go `SkipIncomplete`: orphans (a still or video whose partner is
    /// not in the queue) are consumed with a warning instead of falling
    /// back to single uploads.
    pub skip_incomplete: bool,
    /// Go `IgnoreAppleMetadata`: pair by case-insensitive filename stem
    /// instead of the embedded Apple content identifier. Videos are still
    /// required to carry a still-image-time marker.
    pub ignore_apple_metadata: bool,
}

impl Default for ClassifyOptions {
    /// The gotohp application defaults: pairing off (it is a preference
    /// the user turns on), incomplete-skip on
    /// (`DefaultPreferences` in configmanager.go).
    fn default() -> Self {
        ClassifyOptions {
            pair_live_photos: false,
            skip_incomplete: true,
            ignore_apple_metadata: false,
        }
    }
}

/// Metadata source — port of Go's `LivePhotoMetadataReader` interface,
/// which exists so classification can be tested without real files.
/// Note the video method returns `MovInfo` (not `Option<MovInfo>`): the
/// missing-identifier case travels in `MovInfo::content_identifier`,
/// exactly like Go returning metadata together with
/// `ErrContentIdentifierMissing`.
pub trait LivePhotoMetadataReader {
    fn photo_content_identifier(&self, path: &Path) -> Result<Option<String>>;
    fn video_live_photo_metadata(&self, path: &Path) -> Result<MovInfo>;
}

/// The production reader — port of Go's `fileLivePhotoMetadataReader`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileLivePhotoMetadataReader;

impl LivePhotoMetadataReader for FileLivePhotoMetadataReader {
    fn photo_content_identifier(&self, path: &Path) -> Result<Option<String>> {
        content_identifier_still(path)
    }

    fn video_live_photo_metadata(&self, path: &Path) -> Result<MovInfo> {
        read_video_metadata(path)
    }
}

/// Classify `paths` into upload work items — the frozen-signature entry
/// point. `pair` is the PairLivePhotos preference and
/// `ignore_apple_metadata` the filename-stem override; `skip_incomplete`
/// takes gotohp's application default (`true`). Use
/// [`classify_with_options`] to control all three.
pub fn classify(paths: &[PathBuf], pair: bool, ignore_apple_metadata: bool) -> ClassifiedWork {
    classify_with_options(
        paths,
        &ClassifyOptions {
            pair_live_photos: pair,
            skip_incomplete: true,
            ignore_apple_metadata,
        },
    )
}

/// Port of Go's `ClassifyUploadWork` with the production file reader.
pub fn classify_with_options(paths: &[PathBuf], options: &ClassifyOptions) -> ClassifiedWork {
    classify_with_reader(paths, options, &FileLivePhotoMetadataReader)
}

/// Port of Go's `ClassifyUploadWork(paths, options, reader)`.
pub fn classify_with_reader(
    paths: &[PathBuf],
    options: &ClassifyOptions,
    reader: &dyn LivePhotoMetadataReader,
) -> ClassifiedWork {
    if !options.pair_live_photos {
        return ClassifiedWork {
            items: paths
                .iter()
                .map(|path| WorkItem::Single(path.clone()))
                .collect(),
            warnings: Vec::new(),
        };
    }

    let mut groups: HashMap<String, Candidates> = HashMap::new();
    let mut warnings: Vec<PreflightWarning> = Vec::new();

    for (index, path) in paths.iter().enumerate() {
        match candidate_type(path) {
            Some(CandidateType::Photo) => {
                if options.ignore_apple_metadata {
                    groups
                        .entry(filename_match_key(path))
                        .or_default()
                        .photos
                        .push(Indexed {
                            path: path.clone(),
                            index,
                        });
                    continue;
                }
                match reader.photo_content_identifier(path) {
                    // No identifier: not a candidate; falls back to a
                    // single upload in the assembly pass (Go: `continue`
                    // on ErrContentIdentifierMissing).
                    Ok(None) => continue,
                    Ok(Some(identifier)) => {
                        groups
                            .entry(identifier.to_lowercase())
                            .or_default()
                            .photos
                            .push(Indexed {
                                path: path.clone(),
                                index,
                            });
                    }
                    Err(e) => {
                        warnings.push(metadata_read_warning(path, &e));
                        continue;
                    }
                }
            }
            Some(CandidateType::Video) => {
                let info = match reader.video_live_photo_metadata(path) {
                    Ok(info) => info,
                    Err(e) => {
                        warnings.push(metadata_read_warning(path, &e));
                        continue;
                    }
                };
                // Go: a missing identifier ends candidacy in metadata
                // mode; in stem mode the (identifier-less) metadata is
                // still consulted for the still-image-time check below.
                if info.content_identifier.is_none() && !options.ignore_apple_metadata {
                    continue;
                }
                if !info.has_still_image_time {
                    warnings.push(make_warning(
                        path,
                        &[path.clone()],
                        "missing-still-image-time",
                        "video has a Live Photo content identifier but no still-image-time marker",
                    ));
                    continue;
                }
                let key = if options.ignore_apple_metadata {
                    filename_match_key(path)
                } else {
                    // Safe: the None case either continued above or is
                    // stem mode; reaching here in metadata mode means
                    // an identifier exists.
                    info.content_identifier
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                };
                groups.entry(key).or_default().videos.push(Indexed {
                    path: path.clone(),
                    index,
                });
            }
            None => {}
        }
    }

    // Group resolution order matches Go's sortedLivePhotoIdentifiers:
    // groups are processed in order of their first candidate's index.
    let mut keys: Vec<String> = groups.keys().cloned().collect();
    keys.sort_by_key(|key| groups[key].first_index());

    let mut pairs_by_index: HashMap<usize, WorkItem> = HashMap::new();
    let mut consumed = vec![false; paths.len()];

    for key in &keys {
        let candidates = &groups[key];
        if candidates.photos.len() == 1 && candidates.videos.len() == 1 {
            let photo = &candidates.photos[0];
            let video = &candidates.videos[0];
            let pair_index = photo.index.min(video.index);
            pairs_by_index.insert(
                pair_index,
                WorkItem::LivePhoto {
                    still: photo.path.clone(),
                    video: video.path.clone(),
                    identifier: if options.ignore_apple_metadata {
                        String::new()
                    } else {
                        key.clone()
                    },
                },
            );
            consumed[photo.index] = true;
            consumed[video.index] = true;
            continue;
        }
        if candidates.photos.len() > 1 || candidates.videos.len() > 1 {
            let (code, message) = if options.ignore_apple_metadata {
                (
                    "ambiguous-filename-stem",
                    "multiple photo or video candidates share one filename stem",
                )
            } else {
                (
                    "ambiguous-identifier",
                    "multiple photo or video candidates share one Live Photo identifier",
                )
            };
            let group_paths = candidates.paths();
            warnings.push(make_warning(&group_paths[0], &group_paths, code, message));
            if options.ignore_apple_metadata {
                // Filename matching is an explicit override, so ambiguity
                // must skip every candidate instead of silently falling
                // back to single uploads. (Comment carried from Go.)
                // In identifier mode the candidates are NOT consumed and
                // upload as singles.
                for indexed in candidates.photos.iter().chain(candidates.videos.iter()) {
                    consumed[indexed.index] = true;
                }
            }
            continue;
        }
        if options.skip_incomplete {
            // What remains is a lone still or a lone video.
            let group_paths = candidates.paths();
            for indexed in candidates.photos.iter().chain(candidates.videos.iter()) {
                consumed[indexed.index] = true;
            }
            warnings.push(make_warning(
                &group_paths[0],
                &group_paths,
                "incomplete-live-photo-skipped",
                "Skipped an incomplete Live Photo because its matching file is not in this queue. You can disable this check in Settings.",
            ));
        }
    }

    let mut items = Vec::with_capacity(paths.len());
    for (index, path) in paths.iter().enumerate() {
        if let Some(pair) = pairs_by_index.remove(&index) {
            items.push(pair);
            continue;
        }
        if consumed[index] {
            continue;
        }
        items.push(WorkItem::Single(path.clone()));
    }

    ClassifiedWork { items, warnings }
}

#[derive(Debug, Clone)]
struct Indexed {
    path: PathBuf,
    index: usize,
}

#[derive(Debug, Default)]
struct Candidates {
    photos: Vec<Indexed>,
    videos: Vec<Indexed>,
}

impl Candidates {
    fn first_index(&self) -> usize {
        self.photos
            .iter()
            .chain(self.videos.iter())
            .map(|indexed| indexed.index)
            .min()
            .unwrap_or(usize::MAX)
    }

    /// Go's `candidatePaths`: photos first, then videos, each in
    /// first-seen order.
    fn paths(&self) -> Vec<PathBuf> {
        self.photos
            .iter()
            .chain(self.videos.iter())
            .map(|indexed| indexed.path.clone())
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateType {
    Photo,
    Video,
}

/// Port of Go's `livePhotoCandidateType`, including its extension rules:
/// Go uses `filepath.Ext` (final dot of the final path element, so a file
/// named exactly `.mov` has extension `.mov`), matched case-insensitively.
fn candidate_type(path: &Path) -> Option<CandidateType> {
    match extension_lower(path).as_str() {
        ".heic" | ".heif" | ".jpg" | ".jpeg" => Some(CandidateType::Photo),
        ".mov" => Some(CandidateType::Video),
        _ => None,
    }
}

/// The final dot-suffix of the file name (including the dot), lower-cased;
/// empty when the name has no dot. Mirrors Go's `filepath.Ext` +
/// `strings.ToLower` on the base name.
fn extension_lower(path: &Path) -> String {
    let Some(name) = path.file_name() else {
        return String::new();
    };
    let name = name.to_string_lossy();
    match name.rfind('.') {
        Some(dot) => name[dot..].to_lowercase(),
        None => String::new(),
    }
}

/// Filename-stem pairing key — port of Go's `livePhotoFilenameMatchKey`:
/// the cleaned directory joined with the lower-cased file stem, so two
/// files pair only when they share a directory AND a stem (directories
/// are compared as-is; only the stem is case-folded).
fn filename_match_key(path: &Path) -> String {
    let cleaned = clean_path(path);
    let directory = cleaned.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = match cleaned.file_name() {
        Some(name) => {
            let name = name.to_string_lossy();
            match name.rfind('.') {
                Some(dot) => name[..dot].to_lowercase(),
                None => name.to_lowercase(),
            }
        }
        None => String::new(),
    };
    directory.join(stem).to_string_lossy().into_owned()
}

/// Lexical path cleaning approximating Go's `filepath.Clean` (which
/// `filepath.Join` also applies): `.` components drop out, `..` pops the
/// previous normal component (and is kept when a relative path has
/// nothing to pop).
fn clean_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let last_is_normal =
                    matches!(out.components().next_back(), Some(Component::Normal(_)));
                if last_is_normal {
                    out.pop();
                } else if out.as_os_str().is_empty()
                    || matches!(out.components().next_back(), Some(Component::ParentDir))
                {
                    out.push("..");
                }
                // `..` at a root/prefix is dropped, as in filepath.Clean.
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn metadata_read_warning(path: &Path, error: &crate::Error) -> PreflightWarning {
    make_warning(
        path,
        &[path.to_path_buf()],
        "metadata-read-failed",
        &format!("could not read Live Photo metadata: {error}"),
    )
}

/// Builds the contract-shaped warning from a Go-shaped one: the code is
/// prefixed to the message; a multi-path warning names its first path in
/// `file_path` and appends the full list to the message.
fn make_warning(
    first_path: &Path,
    all_paths: &[PathBuf],
    code: &str,
    message: &str,
) -> PreflightWarning {
    let mut text = format!("{code}: {message}");
    if all_paths.len() > 1 {
        let listed: Vec<String> = all_paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        text.push_str(&format!(" (paths: {})", listed.join(", ")));
    }
    PreflightWarning {
        file_path: first_path.to_path_buf(),
        message: text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use std::collections::HashSet;
    use std::fs;

    const UUID_A: &str = "AAAAAAAA-1111-4111-8111-111111111111";
    const UUID_B: &str = "BBBBBBBB-2222-4222-8222-222222222222";

    /// Fake metadata reader mirroring Go's injected
    /// `LivePhotoMetadataReader` test seam: per-path canned answers.
    #[derive(Default)]
    struct FakeReader {
        photos: HashMap<PathBuf, Option<String>>,
        videos: HashMap<PathBuf, MovInfo>,
        photo_errors: HashSet<PathBuf>,
        video_errors: HashSet<PathBuf>,
    }

    impl LivePhotoMetadataReader for FakeReader {
        fn photo_content_identifier(&self, path: &Path) -> Result<Option<String>> {
            if self.photo_errors.contains(path) {
                return Err(Error::Protocol("fake photo read failure".to_string()));
            }
            Ok(self.photos.get(path).cloned().unwrap_or(None))
        }

        fn video_live_photo_metadata(&self, path: &Path) -> Result<MovInfo> {
            if self.video_errors.contains(path) {
                return Err(Error::Protocol("fake video read failure".to_string()));
            }
            Ok(self.videos.get(path).cloned().unwrap_or_default())
        }
    }

    fn mov_info(identifier: Option<&str>, still: bool) -> MovInfo {
        MovInfo {
            content_identifier: identifier.map(str::to_string),
            has_still_image_time: still,
        }
    }

    fn temp_paths(names: &[&str]) -> Vec<PathBuf> {
        let dir = std::env::temp_dir().join(format!(
            "outh-livephoto-classify-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        names
            .iter()
            .map(|name| {
                let path = dir.join(name);
                fs::write(&path, b"placeholder").expect("write temp file");
                path
            })
            .collect()
    }

    fn opts(pair: bool, skip: bool, ignore: bool) -> ClassifyOptions {
        ClassifyOptions {
            pair_live_photos: pair,
            skip_incomplete: skip,
            ignore_apple_metadata: ignore,
        }
    }

    #[test]
    fn pairing_disabled_makes_everything_single() {
        let paths = temp_paths(&["a.jpg", "a.mov"]);
        let mut reader = FakeReader::default();
        reader.photos.insert(paths[0].clone(), Some(UUID_A.into()));
        reader
            .videos
            .insert(paths[1].clone(), mov_info(Some(UUID_A), true));
        let out = classify_with_reader(&paths, &opts(false, true, false), &reader);
        assert_eq!(
            out.items,
            vec![
                WorkItem::Single(paths[0].clone()),
                WorkItem::Single(paths[1].clone())
            ]
        );
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn metadata_pairing_is_case_insensitive_and_ordered_by_first_seen() {
        // Video listed before the still: the pair item is emitted at the
        // video's position (min index), the photo is not repeated.
        let paths = temp_paths(&["clip.mov", "photo.jpg", "notes.png"]);
        let mut reader = FakeReader::default();
        // Still carries the upper-case form, video the lower-case form.
        reader.photos.insert(
            paths[1].clone(),
            Some("AAAAAAAA-1111-4111-8111-111111111111".into()),
        );
        reader.videos.insert(
            paths[0].clone(),
            mov_info(Some("aaaaaaaa-1111-4111-8111-111111111111"), true),
        );
        let out = classify_with_reader(&paths, &opts(true, true, false), &reader);
        assert_eq!(out.items.len(), 2);
        assert_eq!(
            out.items[0],
            WorkItem::LivePhoto {
                still: paths[1].clone(),
                video: paths[0].clone(),
                identifier: "aaaaaaaa-1111-4111-8111-111111111111".to_string(),
            }
        );
        // .png is not a Live Photo candidate and stays a single.
        assert_eq!(out.items[1], WorkItem::Single(paths[2].clone()));
        assert!(out.warnings.is_empty());
        assert_eq!(out.items[0].primary_path(), paths[1].as_path());
        assert_eq!(out.items[0].paths(), vec![paths[1].as_path(), paths[0].as_path()]);
    }

    #[test]
    fn orphan_becomes_single_when_skip_incomplete_off() {
        let paths = temp_paths(&["lonely.jpg"]);
        let mut reader = FakeReader::default();
        reader.photos.insert(paths[0].clone(), Some(UUID_A.into()));
        let out = classify_with_reader(&paths, &opts(true, false, false), &reader);
        assert_eq!(out.items, vec![WorkItem::Single(paths[0].clone())]);
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn orphan_is_consumed_with_warning_when_skip_incomplete_on() {
        let paths = temp_paths(&["lonely.mov"]);
        let mut reader = FakeReader::default();
        reader
            .videos
            .insert(paths[0].clone(), mov_info(Some(UUID_B), true));
        let out = classify_with_reader(&paths, &opts(true, true, false), &reader);
        assert!(out.items.is_empty());
        assert_eq!(out.warnings.len(), 1);
        assert_eq!(out.warnings[0].file_path, paths[0]);
        assert!(out.warnings[0]
            .message
            .starts_with("incomplete-live-photo-skipped: "));
    }

    #[test]
    fn ambiguous_identifier_falls_back_to_singles_in_metadata_mode() {
        let paths = temp_paths(&["a.jpg", "b.heic", "c.mov"]);
        let mut reader = FakeReader::default();
        reader.photos.insert(paths[0].clone(), Some(UUID_A.into()));
        reader.photos.insert(paths[1].clone(), Some(UUID_A.into()));
        reader
            .videos
            .insert(paths[2].clone(), mov_info(Some(UUID_A), true));
        let out = classify_with_reader(&paths, &opts(true, true, false), &reader);
        assert_eq!(out.items.len(), 3);
        assert!(out.items.iter().all(|i| matches!(i, WorkItem::Single(_))));
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0]
            .message
            .starts_with("ambiguous-identifier: "));
    }

    #[test]
    fn ambiguous_stem_skips_every_candidate_in_stem_mode() {
        // IMG.jpg + IMG.heic share a stem; in stem mode ambiguity must
        // consume everything (no silent single fallback).
        let paths = temp_paths(&["IMG.jpg", "IMG.heic", "IMG.mov"]);
        let mut reader = FakeReader::default();
        reader
            .videos
            .insert(paths[2].clone(), mov_info(None, true));
        let out = classify_with_reader(&paths, &opts(true, true, true), &reader);
        assert!(out.items.is_empty());
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0]
            .message
            .starts_with("ambiguous-filename-stem: "));
    }

    #[test]
    fn stem_mode_pairs_by_stem_with_blank_identifier() {
        // Metadata-free still + identifier-less (but still-time-valid)
        // video, same stem in different cases.
        let paths = temp_paths(&["IMG_0001.JPG", "img_0001.mov"]);
        let mut reader = FakeReader::default();
        reader
            .videos
            .insert(paths[1].clone(), mov_info(None, true));
        let out = classify_with_reader(&paths, &opts(true, true, true), &reader);
        assert_eq!(
            out.items,
            vec![WorkItem::LivePhoto {
                still: paths[0].clone(),
                video: paths[1].clone(),
                identifier: String::new(),
            }]
        );
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn stem_mode_does_not_pair_across_directories() {
        let dir = std::env::temp_dir().join(format!(
            "outh-livephoto-classify-test-{}-dirs",
            std::process::id()
        ));
        let a = dir.join("a").join("IMG.jpg");
        let b = dir.join("b").join("IMG.mov");
        fs::create_dir_all(a.parent().unwrap()).expect("mkdir a");
        fs::create_dir_all(b.parent().unwrap()).expect("mkdir b");
        fs::write(&a, b"x").expect("write a");
        fs::write(&b, b"x").expect("write b");
        let paths = vec![a, b];
        let mut reader = FakeReader::default();
        reader
            .videos
            .insert(paths[1].clone(), mov_info(None, true));
        let out = classify_with_reader(&paths, &opts(true, false, true), &reader);
        assert_eq!(out.items.len(), 2);
        assert!(out.items.iter().all(|i| matches!(i, WorkItem::Single(_))));
    }

    #[test]
    fn video_without_still_image_time_warns_and_uploads_single() {
        let paths = temp_paths(&["clip.mov", "clip.jpg"]);
        let mut reader = FakeReader::default();
        reader.photos.insert(paths[1].clone(), Some(UUID_A.into()));
        reader
            .videos
            .insert(paths[0].clone(), mov_info(Some(UUID_A), false));
        let out = classify_with_reader(&paths, &opts(true, false, false), &reader);
        assert_eq!(out.items.len(), 2);
        assert!(out.items.iter().all(|i| matches!(i, WorkItem::Single(_))));
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0]
            .message
            .starts_with("missing-still-image-time: "));
        assert_eq!(out.warnings[0].file_path, paths[0]);
    }

    #[test]
    fn video_without_identifier_is_silent_single_in_metadata_mode() {
        let paths = temp_paths(&["clip.mov"]);
        let mut reader = FakeReader::default();
        reader
            .videos
            .insert(paths[0].clone(), mov_info(None, true));
        let out = classify_with_reader(&paths, &opts(true, true, false), &reader);
        assert_eq!(out.items, vec![WorkItem::Single(paths[0].clone())]);
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn metadata_read_error_warns_and_uploads_single() {
        let paths = temp_paths(&["broken.jpg"]);
        let mut reader = FakeReader::default();
        reader.photo_errors.insert(paths[0].clone());
        let out = classify_with_reader(&paths, &opts(true, true, false), &reader);
        assert_eq!(out.items, vec![WorkItem::Single(paths[0].clone())]);
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0]
            .message
            .starts_with("metadata-read-failed: could not read Live Photo metadata: "));
    }

    #[test]
    fn classify_fixed_signature_uses_go_default_skip_incomplete() {
        // Uses the production file reader on placeholder files: a plain
        // .jpg without a MakerNote has no identifier, so it stays single;
        // this exercises `classify` itself rather than a fake reader.
        let paths = temp_paths(&["real-plain.jpg"]);
        let out = classify(&paths, true, false);
        assert_eq!(out.items, vec![WorkItem::Single(paths[0].clone())]);
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn filename_key_rules() {
        // Stem is case-folded; directory is not.
        assert_eq!(
            filename_match_key(Path::new("/Photos/Sub/IMG_1.MOV")),
            filename_match_key(Path::new("/Photos/Sub/img_1.jpg"))
        );
        assert_ne!(
            filename_match_key(Path::new("/Photos/A/IMG_1.MOV")),
            filename_match_key(Path::new("/Photos/B/IMG_1.jpg"))
        );
        // Clean semantics: `..` resolves lexically before keying.
        assert_eq!(
            filename_match_key(Path::new("/Photos/x/../Sub/IMG_1.mov")),
            filename_match_key(Path::new("/Photos/Sub/IMG_1.jpg"))
        );
    }
}
