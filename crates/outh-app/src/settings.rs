//! Settings section: every persisted `Preferences` field (CONTRACT.md
//! `config.rs`), edited in place and persisted on change — mirroring Go,
//! where every `ConfigManager.SetX` saves immediately. Album fields are
//! deliberately absent here: in Go they are session-only choices, so they
//! live in the Upload section's state, not in persisted settings.

use windows_reactor::*;

use crate::{Message, OuthApp, PrefBool};

pub fn view(app: &OuthApp, context: &mut ViewContext<OuthApp>) -> View {
    let prefs = &app.prefs;
    // The gallery's toggle-switch page pattern: a local helper closure
    // building one switch per boolean preference.
    let toggle = |is_on: bool, label: &str, field: PrefBool| {
        ToggleSwitch::new()
            .is_on(is_on)
            .on_toggled(context.callback(move |value: bool| {
                Message::PrefBoolChanged(field, value)
            }))
            .header(label)
            .on_content("On")
            .off_content("Off")
    };

    let mut children: Vec<View> = Vec::new();
    children.push(TextBlock::new().text("Settings").font_size(20.0).into());
    if let Some(error) = &app.store_error {
        children.push(
            TextBlock::new()
                .text(error.clone())
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        );
    }
    if let Some(note) = &app.settings_note {
        children.push(TextBlock::new().text(note.clone()).into());
    }

    children.push(
        toggle(prefs.use_quota, "Use quota", PrefBool::UseQuota).into(),
    );
    children.push(
        TextBlock::new()
            .text(
                "Use quota (uploads count toward your storage). Off = uploads claim a \
                 Pixel XL identity and may not count — unofficial, can stop working.",
            )
            .text_wrapping(TextWrapping::Wrap)
            .opacity(0.8)
            .into(),
    );
    children.push(
        toggle(
            prefs.saver,
            "Storage saver quality (Pixel 2 identity)",
            PrefBool::Saver,
        )
        .into(),
    );
    children.push(
        toggle(
            prefs.recursive,
            "Include subfolders when a folder is added",
            PrefBool::Recursive,
        )
        .into(),
    );
    children.push(
        toggle(
            prefs.force_upload,
            "Force upload (skip the already-in-library check)",
            PrefBool::ForceUpload,
        )
        .into(),
    );
    children.push(
        toggle(
            prefs.pair_live_photos,
            "Pair Apple Live Photos (still + video)",
            PrefBool::PairLivePhotos,
        )
        .into(),
    );
    children.push(
        toggle(
            prefs.skip_incomplete_live_photos,
            "Skip incomplete Live Photos (missing a component)",
            PrefBool::SkipIncompleteLivePhotos,
        )
        .into(),
    );
    children.push(
        toggle(
            prefs.update_existing_to_live,
            "Update existing photos to Live Photos when the video arrives",
            PrefBool::UpdateExistingToLive,
        )
        .into(),
    );
    children.push(
        toggle(
            prefs.delete_from_host,
            "Delete local file after a confirmed upload",
            PrefBool::DeleteFromHost,
        )
        .into(),
    );
    children.push(
        TextBlock::new()
            .text(
                "Deleting originals is destructive: the local file is removed only after \
                 Google confirms the upload, as in gotohp. Leave this off unless you are sure.",
            )
            .text_wrapping(TextWrapping::Wrap)
            .opacity(0.8)
            .into(),
    );
    children.push(
        toggle(
            prefs.disable_unsupported_filter,
            "Upload files with unsupported extensions too",
            PrefBool::DisableUnsupportedFilter,
        )
        .into(),
    );
    children.push(
        toggle(
            prefs.set_date_from_filename,
            "Take the capture date from the file name when it contains one",
            PrefBool::SetDateFromFilename,
        )
        .into(),
    );

    children.push(TextBlock::new().text("Upload threads").into());
    children.push(
        NumberBox::new()
            .minimum(1.0)
            .maximum(16.0)
            .value(Some(prefs.upload_threads as f64))
            .on_value_changed(context.callback(Message::PrefThreadsChanged))
            .into(),
    );

    children.push(TextBlock::new().text("Proxy").into());
    children.push(
        TextBox::new(prefs.proxy.clone())
            .placeholder_text("Leave empty for a direct connection, e.g. http://127.0.0.1:8080")
            .on_text_changed(context.callback(|value: std::rc::Rc<str>| {
                Message::PrefProxyChanged(value.to_string())
            }))
            .into(),
    );

    children.push(TextBlock::new().text("Exclude pattern").into());
    children.push(
        TextBox::new(prefs.exclude_pattern.clone())
            .placeholder_text("Regular expression for file names to skip")
            .on_text_changed(context.callback(|value: std::rc::Rc<str>| {
                Message::PrefExcludePatternChanged(value.to_string())
            }))
            .into(),
    );

    ScrollViewer::new()
        .content(
            Border::new()
                .padding(Thickness::new(24.0, 24.0, 24.0, 24.0))
                .content(StackPanel::new().spacing(10.0).children(children)),
        )
        .into()
}
