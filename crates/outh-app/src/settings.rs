//! Settings section (spec §6): every persisted `Preferences` field,
//! grouped into cards of settings rows — title + honest description on
//! the left, the bare control on the right (Fluent settings pattern,
//! V-27). Edits persist on change, mirroring Go, where every
//! `ConfigManager.SetX` saves immediately. Album fields are deliberately
//! absent: in Go they are session-only, so they live in Upload.
//!
//! Copy rule (R-8): descriptions state what core actually does —
//! including the uncomfortable parts (delete-originals also deletes
//! files that were already in the library; the exclude field is an
//! exact folder-name match, not a pattern language).

use windows_reactor::*;

use crate::theme;
use crate::{Message, OuthApp, PrefBool};

pub fn view(app: &OuthApp, context: &mut ViewContext<OuthApp>) -> View {
    let prefs = &app.prefs;
    // One bare switch per boolean preference: no header, no On/Off text
    // (the settings_row carries the label and explanation).
    let toggle = |is_on: bool, field: PrefBool| -> View {
        ToggleSwitch::new()
            .is_on(is_on)
            .on_toggled(context.callback(move |value: bool| {
                Message::PrefBoolChanged(field, value)
            }))
            .into()
    };

    let mut children: Vec<View> = Vec::new();
    children.push(theme::page_title("Settings"));
    // The store error renders globally above every page (main.rs, §2).
    if let Some(note) = &app.settings_note {
        children.push(theme::note_bar(
            note,
            context.message(Message::DismissSettingsNote),
        ));
    }

    // ---- Storage & identity ----
    children.push(theme::section_title("Storage & identity"));
    children.push(theme::card(vec![
        theme::settings_row(
            "Use quota",
            "Uploads count toward your Google storage. Off = uploads claim a \
             Pixel XL identity and may not count toward storage — unofficial, \
             and it can stop working at any time.",
            toggle(prefs.use_quota, PrefBool::UseQuota),
        ),
        theme::settings_row(
            "Storage saver",
            "Uploads claim a Pixel 2 identity; photos are compressed to high \
             quality and may count differently — unofficial.",
            toggle(prefs.saver, PrefBool::Saver),
        ),
    ]));

    // ---- Upload behaviour ----
    children.push(theme::section_title("Upload behaviour"));
    children.push(theme::card(vec![
        theme::settings_row(
            "Include subfolders",
            "When a folder is added, files inside its subfolders are queued too.",
            toggle(prefs.recursive, PrefBool::Recursive),
        ),
        theme::settings_row(
            "Force upload",
            "Skip the already-in-library check and upload every file again, \
             even ones Google Photos already has.",
            toggle(prefs.force_upload, PrefBool::ForceUpload),
        ),
        theme::settings_row(
            "Delete local file after upload",
            "Deletes the local file once Google confirms the upload — \
             including files that turn out to already be in your library, \
             which are deleted locally without being uploaded again. \
             Deletion is permanent.",
            toggle(prefs.delete_from_host, PrefBool::DeleteFromHost),
        ),
    ]));

    // ---- Live Photos ----
    children.push(theme::section_title("Live Photos"));
    children.push(theme::card(vec![
        theme::settings_row(
            "Pair Apple Live Photos",
            "Match a still photo with its video by Apple's content \
             identifier and upload them as one Live Photo.",
            toggle(prefs.pair_live_photos, PrefBool::PairLivePhotos),
        ),
        theme::settings_row(
            "Skip incomplete Live Photos",
            "Skip a Live Photo when one of its two files — the still or the \
             video — is missing.",
            toggle(
                prefs.skip_incomplete_live_photos,
                PrefBool::SkipIncompleteLivePhotos,
            ),
        ),
        theme::settings_row(
            "Update existing photos to Live Photos",
            "When the video half of a Live Photo arrives later, convert the \
             photo already in your library into a Live Photo.",
            toggle(prefs.update_existing_to_live, PrefBool::UpdateExistingToLive),
        ),
    ]));

    // ---- Files ----
    children.push(theme::section_title("Files"));
    children.push(theme::card(vec![
        theme::settings_row(
            "Set date from file name",
            "When a file name contains a date and time, use it as the \
             photo's capture date; otherwise the file's own timestamp is used.",
            toggle(prefs.set_date_from_filename, PrefBool::SetDateFromFilename),
        ),
        theme::settings_row(
            "Include unsupported file types",
            "Also queue files whose extensions Google Photos may not \
             accept. Off = only known photo and video types are queued.",
            // Presented positively: the switch is the inverse of core's
            // `disable_unsupported_filter` flag.
            ToggleSwitch::new()
                .is_on(!prefs.disable_unsupported_filter)
                .on_toggled(context.callback(|value: bool| {
                    Message::PrefBoolChanged(PrefBool::DisableUnsupportedFilter, !value)
                }))
                .into(),
        ),
    ]));

    // ---- Advanced ----
    children.push(theme::section_title("Advanced"));
    children.push(theme::card(vec![
        theme::settings_row(
            "Upload threads",
            "Parallel uploads (1–16).",
            NumberBox::new()
                .minimum(1.0)
                .maximum(16.0)
                .value(Some(prefs.upload_threads as f64))
                .on_value_changed(context.callback(Message::PrefThreadsChanged))
                .into(),
        ),
        theme::settings_row(
            "Proxy",
            "http(s)://host:port — leave empty for direct.",
            TextBox::new(prefs.proxy.clone())
                .placeholder_text("http://127.0.0.1:8080")
                .on_text_changed(context.callback(|value: std::rc::Rc<str>| {
                    Message::PrefProxyChanged(value.to_string())
                }))
                .into(),
        ),
        theme::settings_row(
            "Exclude folders by name",
            "Folders with exactly this name are skipped (not a regular \
             expression).",
            TextBox::new(prefs.exclude_pattern.clone())
                .placeholder_text("Exact folder name")
                .on_text_changed(context.callback(|value: std::rc::Rc<str>| {
                    Message::PrefExcludePatternChanged(value.to_string())
                }))
                .into(),
        ),
    ]));

    ScrollViewer::new()
        .content(
            Border::new()
                .padding(Thickness::new(
                    theme::PAGE_PADDING_X,
                    theme::PAGE_PADDING_TOP,
                    theme::PAGE_PADDING_X,
                    theme::SPACE_XL,
                ))
                .content(
                    StackPanel::new()
                        .spacing(theme::SPACE_XL)
                        .max_width(theme::CONTENT_MAX_WIDTH)
                        .horizontal_alignment(HorizontalAlignment::Left)
                        .children(children),
                ),
        )
        .into()
}
