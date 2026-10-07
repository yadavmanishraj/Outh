//! Capture-time extraction from filenames — port of gotohp
//! `core/filename_parser.go`.
//!
//! Used when `SetDateFromFilename` is on: gotohp has *no EXIF dates anywhere*;
//! the upload timestamp is the file mtime, or a timestamp parsed out of the
//! filename by the four regex patterns below (tried in Go's priority order).
//!
//! ## Local-time handling (read before touching)
//!
//! Go parses the date/time patterns with `time.ParseInLocation(..., time.Local)`
//! and validates the unix-ms pattern's year in local time too. Rust's `std`
//! exposes no local UTC offset and outh-core is platform-independent with a
//! frozen dependency list (no tz database crate), so the offset is an
//! explicit parameter of the internal parser:
//!
//! - [`parse_timestamp_from_filename`] passes the offset from
//!   [`local_utc_offset_secs`], which is currently **0 (UTC)**. On a machine
//!   whose local zone is not UTC this shifts filename-derived timestamps by
//!   the local offset versus Go. Follow-up for integration: supply the real
//!   offset (e.g. Win32 `GetTimeZoneInformation` in the app, injected here, or
//!   a tz crate added to the workspace) — the parser math is already
//!   offset-correct and unit-tested with a +05:45 offset.
//! - The unix-ms pattern denotes an absolute instant, so its returned value is
//!   offset-independent (Go converts it to local time only to read the year
//!   for range validation; the boundary difference is at most one year-edge
//!   and is ignored here — validation uses the UTC year).
//!
//! Range validation matches Go: the resulting year must lie in
//! `1990..=current_year + 1`, and an unparseable or out-of-range candidate
//! falls through to the next pattern (`continue` in Go), never aborting.

use std::path::Path;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

/// Which shape a pattern captures.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PatternKind {
    /// Captures date + time (groups 1..=6 are Y M D h m s).
    DateTime,
    /// Captures a date, with an optional time group; missing time means
    /// 12:00:00 (Go's default for the dashed pattern).
    DateOptionalTime,
    /// Captures a 13-digit unix-milliseconds value in group 1.
    UnixMs,
}

struct FilenamePattern {
    re: Regex,
    kind: PatternKind,
}

fn patterns() -> &'static [FilenamePattern] {
    static PATTERNS: OnceLock<Vec<FilenamePattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        // Regexes copied verbatim from filename_parser.go.
        let defs: [(&str, PatternKind); 4] = [
            // YYYYMMDD[_-]HHMMSS - e.g. 20240709_182027.mp4, PXL_20231123_182518628.jpg
            (r"(\d{4})(\d{2})(\d{2})[_-](\d{2})(\d{2})(\d{2})\d*", PatternKind::DateTime),
            // YYYY-MM-DD[sep HHMMSS] - e.g. 2022-10-24-150226287.mp4, Screenshot 2026-02-13 093505.png
            (
                r"(\d{4})-(\d{1,2})-(\d{1,2})(?:[ _-](\d{2})(\d{2})(\d{2})\d*)?",
                PatternKind::DateOptionalTime,
            ),
            // [non-digit]YYYYMMDDHHMMSS[non-digit] - e.g. lv_7324034615860006160_20240617193045.mp4
            (
                r"(?:^|[^0-9])(\d{4})(\d{2})(\d{2})(\d{2})(\d{2})(\d{2})(?:[^0-9]|$)",
                PatternKind::DateTime,
            ),
            // Unix milliseconds - e.g. FaceApp_1658848332262.jpg (covers 2001-2033)
            (r"(?:^|[^0-9])(1\d{12})(?:[^0-9]|$)", PatternKind::UnixMs),
        ];
        defs.iter()
            .map(|(src, kind)| FilenamePattern {
                re: Regex::new(src).expect("filename pattern must compile"),
                kind: *kind,
            })
            .collect()
    })
}

/// The local UTC offset applied to filename-derived wall times.
/// Currently 0 (UTC) — see the module docs for why and for the follow-up.
fn local_utc_offset_secs() -> i64 {
    0
}

/// Current (UTC) year, used for Go's `time.Now().Year()+1` upper bound.
fn current_year() -> i64 {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86_400) as i64)
        .unwrap_or(0);
    civil_from_days(days).0
}

/// Extract a capture timestamp (unix seconds) from a filename or path —
/// Rust spelling of Go `parseTimestampFromFilename`. Only the base name is
/// examined, exactly like Go's `filepath.Base`.
pub fn parse_timestamp_from_filename(path_or_name: &str) -> Option<i64> {
    let base = Path::new(path_or_name)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_or_name.to_string());
    parse_with_offset(&base, local_utc_offset_secs(), current_year())
}

/// Internal parser with explicit local offset and "current year" so tests
/// are deterministic. `offset_secs` is added to UTC to get local wall time:
/// `unix = wall_as_utc - offset_secs`.
fn parse_with_offset(base: &str, offset_secs: i64, now_year: i64) -> Option<i64> {
    for pat in patterns() {
        let caps = match pat.re.captures(base) {
            Some(c) => c,
            None => continue,
        };

        if pat.kind == PatternKind::UnixMs {
            let ms: i64 = match caps[1].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let (year, _, _) = civil_from_days(ms.div_euclid(86_400_000));
            if year < 1990 || year > now_year + 1 {
                continue;
            }
            return Some(ms.div_euclid(1000));
        }

        let year: i64 = caps[1].parse().ok()?;
        let month: i64 = caps[2].parse().ok()?;
        let day: i64 = caps[3].parse().ok()?;

        // Go: patterns with hasTime always carry the time; the dashed
        // pattern uses its optional group when present, else 12:00:00.
        let (hour, minute, second) = match pat.kind {
            PatternKind::DateTime => (
                caps[4].parse::<i64>().ok()?,
                caps[5].parse::<i64>().ok()?,
                caps[6].parse::<i64>().ok()?,
            ),
            PatternKind::DateOptionalTime => {
                if let Some(h) = caps.get(4) {
                    (
                        h.as_str().parse::<i64>().ok()?,
                        caps[5].parse::<i64>().ok()?,
                        caps[6].parse::<i64>().ok()?,
                    )
                } else {
                    (12, 0, 0)
                }
            }
            PatternKind::UnixMs => unreachable!(),
        };

        // Go's time.ParseInLocation rejects out-of-range components (bad
        // month, Feb 30, hour 24, ...); mirror that by validating here.
        if !valid_civil(year, month, day, hour, minute, second) {
            continue;
        }
        if year < 1990 || year > now_year + 1 {
            continue;
        }

        let wall_as_utc =
            days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second;
        return Some(wall_as_utc - offset_secs);
    }
    None
}

fn valid_civil(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> bool {
    if !(1..=12).contains(&month) {
        return false;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = [
        31,
        if leap { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ][(month - 1) as usize];
    day >= 1 && day <= days_in_month && hour < 24 && minute < 60 && second < 60
}

/// Days since 1970-01-01 for a proleptic Gregorian date
/// (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month, day) for days since 1970-01-01 (Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fixed "now" (2026) so Go's `Year()+1` bound is deterministic.
    const NOW_YEAR: i64 = 2026;
    // Asia/Kathmandu offset, used to prove the offset math.
    const KTM: i64 = 5 * 3600 + 45 * 60;

    fn parse(name: &str) -> Option<i64> {
        parse_with_offset(name, 0, NOW_YEAR)
    }

    #[test]
    fn table_from_go_pattern_examples() {
        // Every name/expectation pair is taken from the worked examples in
        // filename_parser.go's comments; expected values are the UTC unix
        // seconds for the wall time shown (offset 0). Go has no dedicated
        // filename_parser_test.go — these comment examples are its table.
        let cases: [(&str, i64); 6] = [
            ("20240709_182027.mp4", 1_720_549_227),
            ("PXL_20231123_182518628.jpg", 1_700_763_918),
            ("2022-10-24-150226287.mp4", 1_666_623_746),
            ("Screenshot 2026-02-13 093505.png", 1_770_975_305),
            ("lv_7324034615860006160_20240617193045.mp4", 1_718_652_645),
            ("FaceApp_1658848332262.jpg", 1_658_848_332),
        ];
        for (name, want) in cases {
            assert_eq!(parse(name), Some(want), "case {name}");
        }
    }

    #[test]
    fn dashed_date_without_time_defaults_to_noon() {
        // Go defaults the time to 12:00:00 for the dashed pattern.
        assert_eq!(parse("IMG_2022-10-24.jpg"), Some(1_666_612_800));
    }

    #[test]
    fn only_base_name_is_examined() {
        assert_eq!(
            parse_with_offset("/some/dir/20240709_182027.mp4", 0, NOW_YEAR),
            Some(1_720_549_227)
        );
        // Public entry point agrees for a bare name.
        assert_eq!(
            parse_timestamp_from_filename("20240709_182027.mp4"),
            Some(1_720_549_227)
        );
    }

    #[test]
    fn local_offset_shifts_wall_time_patterns() {
        // The same wall time in Kathmandu (+05:45) is 20700 s earlier in UTC.
        assert_eq!(
            parse_with_offset("20240709_182027.mp4", KTM, NOW_YEAR),
            Some(1_720_549_227 - KTM)
        );
        // The unix-ms pattern is an absolute instant: offset-independent.
        assert_eq!(
            parse_with_offset("FaceApp_1658848332262.jpg", KTM, NOW_YEAR),
            Some(1_658_848_332)
        );
    }

    #[test]
    fn rejects_unparseable_and_out_of_range() {
        assert_eq!(parse("photo.jpg"), None);
        assert_eq!(parse("holiday.mp4"), None);
        // Month 13: rejected by Go's time.Parse equivalent validation.
        assert_eq!(parse("20241301_120000.mp4"), None);
        // Feb 30 does not exist.
        assert_eq!(parse("20240230_120000.mp4"), None);
        // Year below 1990 falls through every pattern.
        assert_eq!(parse("19890101_120000.jpg"), None);
        // Year beyond now+1 falls through too.
        assert_eq!(parse("20990101_120000.jpg"), None);
    }

    #[test]
    fn civil_conversions_round_trip() {
        for &(y, m, d) in &[
            (1970, 1, 1),
            (2000, 2, 29),
            (2024, 7, 9),
            (1999, 12, 31),
            (2033, 5, 18),
        ] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }
}
