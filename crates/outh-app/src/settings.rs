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
    let toggle = |name: &str, is_on: bool, field: PrefBool| -> View {
        ToggleSwitch::new()
                .width(44.0)
                .min_width(0.0)
            .on_content("")
            .off_content("")
            .automation_name(name)
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

    // ---- Appearance ----
    children.push(theme::section_title("Appearance"));
    // Individual RadioButtons, not a RadioButtons group: the group's
    // selection-index round-trip lost the click on the first item
    // (verified on the laptop — choosing System never persisted), while
    // per-button Checked events are deterministic.
    let theme_radio = |label: &str, value: &'static str, checked: bool| -> View {
        RadioButton::new()
            .group_name("outh-theme")
            .content(label)
            .is_checked(Some(checked))
            .on_checked(context.callback(move |_checked: Option<bool>| {
                Message::ThemeChosen(value)
            }))
            .into()
    };
    children.push(theme::card(vec![
        theme::strong("Theme"),
        theme_radio("System", "system", prefs.theme != "light" && prefs.theme != "dark"),
        theme_radio("Light", "light", prefs.theme == "light"),
        theme_radio("Dark", "dark", prefs.theme == "dark"),
        theme::caption("System follows your Windows setting."),
    ]));

    // ---- Storage & identity ----
    children.push(theme::section_title("Storage & identity"));
    children.push(theme::card(vec![
        theme::settings_row(
            "Use quota",
            "Uploads count toward your Google storage. Off = uploads claim a \
             Pixel XL identity and may not count toward storage — unofficial, \
             and it can stop working at any time.",
            toggle("Use quota", prefs.use_quota, PrefBool::UseQuota),
        ),
        theme::settings_row(
            "Storage saver",
            "Uploads claim a Pixel 2 identity; photos are compressed to high \
             quality and may count differently — unofficial.",
            toggle("Storage saver", prefs.saver, PrefBool::Saver),
        ),
    ]));

    // ---- Upload behaviour ----
    children.push(theme::section_title("Upload behaviour"));
    children.push(theme::card(vec![
        theme::settings_row(
            "Include subfolders",
            "When a folder is added, files inside its subfolders are queued too.",
            toggle("Include subfolders", prefs.recursive, PrefBool::Recursive),
        ),
        theme::settings_row(
            "Force upload",
            "Skip the already-in-library check and upload every file again, \
             even ones Google Photos already has.",
            toggle("Force upload", prefs.force_upload, PrefBool::ForceUpload),
        ),
        theme::settings_row(
            "Delete local file after upload",
            "Deletes the local file once Google confirms the upload — \
             including files that turn out to already be in your library, \
             which are deleted locally without being uploaded again. \
             Deletion is permanent.",
            toggle("Delete local file after upload", prefs.delete_from_host, PrefBool::DeleteFromHost),
        ),
    ]));

    // ---- Live Photos ----
    children.push(theme::section_title("Live Photos"));
    children.push(theme::card(vec![
        theme::settings_row(
            "Pair Apple Live Photos",
            "Match a still photo with its video by Apple's content \
             identifier and upload them as one Live Photo.",
            toggle("Pair Apple Live Photos", prefs.pair_live_photos, PrefBool::PairLivePhotos),
        ),
        theme::settings_row(
            "Skip incomplete Live Photos",
            "Skip a Live Photo when one of its two files — the still or the \
             video — is missing. Only applies while pairing is on.",
            // Dependent on pairing (R-24): disabled while Pair is off.
            ToggleSwitch::new()
                .width(44.0)
                .min_width(0.0)
                .on_content("")
                .off_content("")
                .automation_name("Skip incomplete Live Photos")
                .is_enabled(prefs.pair_live_photos)
                .is_on(prefs.skip_incomplete_live_photos)
                .on_toggled(context.callback(|value: bool| {
                    Message::PrefBoolChanged(PrefBool::SkipIncompleteLivePhotos, value)
                }))
                .into(),
        ),
        theme::settings_row(
            "Update existing photos to Live Photos",
            "When the video half of a Live Photo arrives later, convert the \
             photo already in your library into a Live Photo. Only applies \
             while pairing is on.",
            // Dependent on pairing (R-24): disabled while Pair is off.
            ToggleSwitch::new()
                .width(44.0)
                .min_width(0.0)
                .on_content("")
                .off_content("")
                .automation_name("Update existing photos to Live Photos")
                .is_enabled(prefs.pair_live_photos)
                .is_on(prefs.update_existing_to_live)
                .on_toggled(context.callback(|value: bool| {
                    Message::PrefBoolChanged(PrefBool::UpdateExistingToLive, value)
                }))
                .into(),
        ),
    ]));

    // ---- Files ----
    children.push(theme::section_title("Files"));
    children.push(theme::card(vec![
        theme::settings_row(
            "Set date from file name",
            "When a file name contains a date and time, use it as the \
             photo's capture date; otherwise the file's own timestamp is used.",
            toggle("Set date from file name", prefs.set_date_from_filename, PrefBool::SetDateFromFilename),
        ),
        theme::settings_row(
            "Include unsupported file types",
            "Also queue files whose extensions Google Photos may not \
             accept. Off = only known photo and video types are queued.",
            // Presented positively: the switch is the inverse of core's
            // `disable_unsupported_filter` flag.
            ToggleSwitch::new()
                .width(44.0)
                .min_width(0.0)
                .on_content("")
                .off_content("")
                .automation_name("Include unsupported file types")
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
                .automation_name("Upload threads")
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
                .automation_name("Proxy")
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
                .automation_name("Exclude folders by name")
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
                        .children(children),
                ),
        )
        .into()
}
