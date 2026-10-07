#![windows_subsystem = "windows"]

//! Outh — Google Photos uploader for Windows, on windows-reactor (WinUI 3).
//!
//! A single root component, `OuthApp`, owns all UI state (the reactor
//! model: state mutates only in `update`, callbacks only enqueue
//! messages). Sections are plain view functions in their own modules:
//! `upload.rs`, `accounts.rs`, `settings.rs`. Cross-thread work reports
//! back as messages — uploads via the `UploadReporter` bridge in
//! `reporter.rs`, the sign-in exchange via `spawn_background` in
//! `accounts.rs`.

mod accounts;
mod protector;
mod reporter;
mod settings;
mod store;
mod upload;

use std::path::PathBuf;
use std::rc::Rc;

use outh_core::config::{Account, ConfigService, Preferences};
use outh_core::credential::Credential;
use outh_core::types::{AlbumStatus, FileResult, Outcome, PreflightWarning, ThreadStatus};
use windows_pickers::{FolderPicker, OpenFilePicker};
use windows_reactor::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Upload,
    Accounts,
    Settings,
}

/// Album choice in the Upload section (session-only, as in Go).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AlbumMode {
    None,
    Named,
    Auto,
}

impl AlbumMode {
    fn index(self) -> usize {
        match self {
            AlbumMode::None => 0,
            AlbumMode::Named => 1,
            AlbumMode::Auto => 2,
        }
    }

    fn from_index(index: Option<usize>) -> AlbumMode {
        match index {
            Some(1) => AlbumMode::Named,
            Some(2) => AlbumMode::Auto,
            _ => AlbumMode::None,
        }
    }
}

/// Identifies one boolean `Preferences` field, so all settings toggles
/// share a single message + handler.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PrefBool {
    UseQuota,
    Saver,
    Recursive,
    ForceUpload,
    PairLivePhotos,
    SkipIncompleteLivePhotos,
    UpdateExistingToLive,
    DeleteFromHost,
    DisableUnsupportedFilter,
    SetDateFromFilename,
}

impl PrefBool {
    fn apply(self, prefs: &mut Preferences, value: bool) {
        match self {
            PrefBool::UseQuota => prefs.use_quota = value,
            PrefBool::Saver => prefs.saver = value,
            PrefBool::Recursive => prefs.recursive = value,
            PrefBool::ForceUpload => prefs.force_upload = value,
            PrefBool::PairLivePhotos => prefs.pair_live_photos = value,
            PrefBool::SkipIncompleteLivePhotos => prefs.skip_incomplete_live_photos = value,
            PrefBool::UpdateExistingToLive => prefs.update_existing_to_live = value,
            PrefBool::DeleteFromHost => prefs.delete_from_host = value,
            PrefBool::DisableUnsupportedFilter => prefs.disable_unsupported_filter = value,
            PrefBool::SetDateFromFilename => prefs.set_date_from_filename = value,
        }
    }
}

pub enum Message {
    // Navigation (payload: the NavigationView item tag)
    NavigateTag(Option<String>),

    // Upload section
    PathDraftChanged(String),
    AddPathDraft,
    AddFiles,
    AddFolders,
    PathsPicked(Vec<PathBuf>),
    PickerFailed(String),
    RemovePath(usize),
    ClearPaths,
    AlbumModeChanged(Option<usize>),
    AlbumNameChanged(String),
    StartUpload,
    CancelUpload,

    // Upload run — sent by the reporter bridge / upload thread
    UploadStarted(usize),
    UploadStopped,
    UploadTotalBytes(u64),
    UploadTotalBytesDelta(i64),
    UploadWarning(PreflightWarning),
    UploadThreadStatus(ThreadStatus),
    UploadFileResult(FileResult),
    UploadAlbumProgress(AlbumStatus),
    UploadAlbumComplete(AlbumStatus),
    UploadAlbumError(String, String),
    UploadRunEnded(String),

    // Accounts section
    OAuthTokenChanged(String),
    RawCredentialChanged(String),
    OpenSignInPage,
    ConnectAccount,
    AccountConnected(Result<(String, String, bool), String>),
    ImportRawCredential,
    SetActiveAccount(String),
    RemoveAccount(String),

    // Settings section
    PrefBoolChanged(PrefBool, bool),
    PrefThreadsChanged(Option<f64>),
    PrefProxyChanged(String),
    PrefExcludePatternChanged(String),
}

/// One worker's latest status, for the per-worker rows.
pub struct WorkerRow {
    pub worker: usize,
    pub stage: String,
    pub file: String,
    pub bytes_uploaded: u64,
    pub bytes_total: u64,
    pub attempt: u32,
}

/// One finished file, for the results list.
pub struct ResultRow {
    pub file: String,
    pub outcome: String,
    pub message: Option<String>,
}

pub struct OuthApp {
    pub section: Section,

    // Config (loaded at startup; `store` is None only if loading failed,
    // in which case the app runs on defaults and shows `store_error`).
    pub store: Option<ConfigService>,
    pub store_error: Option<String>,
    pub accounts: Vec<Account>,
    pub active_email: Option<String>,
    pub prefs: Preferences,

    // Upload section state
    pub paths: Vec<PathBuf>,
    pub path_draft: String,
    pub album_mode: AlbumMode,
    pub album_name: String,
    pub running: bool,
    pub cancel_requested: bool,
    pub cancel_token: Option<outh_core::types::CancellationToken>,
    pub total_files: usize,
    pub total_bytes: u64,
    pub workers: Vec<WorkerRow>,
    pub results: Vec<ResultRow>,
    pub warnings: Vec<String>,
    pub album_status: Option<String>,
    pub upload_note: Option<String>,

    // Accounts section state
    pub oauth_token: String,
    pub raw_credential: String,
    pub auth_busy: bool,
    pub account_note: Option<String>,

    // Settings section state
    pub settings_note: Option<String>,
}

impl OuthApp {
    /// Re-reads accounts / active account / preferences from the store
    /// after a mutation (the store persists on every write).
    fn refresh_from_store(&mut self) {
        if let Some(service) = &self.store {
            let config = service.config().clone();
            self.accounts = config.accounts;
            self.active_email = config.active_email;
            self.prefs = config.preferences;
        }
    }

    fn persist_preferences(&mut self) {
        if let Some(service) = &mut self.store {
            match store::save_preferences(service, &self.prefs) {
                Ok(()) => self.settings_note = None,
                Err(error) => {
                    self.settings_note = Some(format!("Could not save settings: {error}"))
                }
            }
        }
    }

    fn add_account(&mut self, account: Account) {
        let email = account.email.clone();
        match &mut self.store {
            Some(service) => match store::add_account(service, account) {
                Ok(()) => {
                    // A newly added account becomes the active one, as in
                    // gotohp (the GUI selects the account it just added).
                    let _ = store::set_active(service, &email);
                    self.refresh_from_store();
                    self.account_note = Some(format!("Added account {email}."));
                }
                Err(error) => {
                    self.account_note = Some(format!("Could not save the account: {error}"));
                }
            },
            None => {
                self.account_note =
                    Some("Config could not be loaded, so the account was not saved.".to_string());
            }
        }
    }
}

impl Component for OuthApp {
    type Message = Message;
    type Input = ();

    fn create(_input: &(), _context: &ComponentContext<Self>) -> Self {
        let (store, store_error) = store::load();
        let mut app = Self {
            section: Section::Upload,
            store,
            store_error,
            accounts: Vec::new(),
            active_email: None,
            prefs: Preferences::default(),
            paths: Vec::new(),
            path_draft: String::new(),
            album_mode: AlbumMode::None,
            album_name: String::new(),
            running: false,
            cancel_requested: false,
            cancel_token: None,
            total_files: 0,
            total_bytes: 0,
            workers: Vec::new(),
            results: Vec::new(),
            warnings: Vec::new(),
            album_status: None,
            upload_note: None,
            oauth_token: String::new(),
            raw_credential: String::new(),
            auth_busy: false,
            account_note: None,
            settings_note: None,
        };
        app.refresh_from_store();
        // Album choice is session state, initialised from the loaded
        // preferences (Go keeps it out of the persisted file; the Rust
        // Preferences struct carries the fields, so honour them at start).
        if app.prefs.album_auto_mode {
            app.album_mode = AlbumMode::Auto;
        } else if !app.prefs.album_name.trim().is_empty() {
            app.album_mode = AlbumMode::Named;
            app.album_name = app.prefs.album_name.clone();
        }
        app
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            Message::NavigateTag(tag) => {
                match tag.as_deref() {
                    Some("upload") => self.section = Section::Upload,
                    Some("accounts") => self.section = Section::Accounts,
                    Some("settings") => self.section = Section::Settings,
                    _ => {}
                }
            }

            // ---- Upload section ----
            Message::PathDraftChanged(value) => self.path_draft = value,
            Message::AddPathDraft => {
                let draft = self.path_draft.trim().to_string();
                if !draft.is_empty() {
                    let path = PathBuf::from(draft);
                    if !self.paths.contains(&path) {
                        self.paths.push(path);
                    }
                    self.path_draft.clear();
                }
            }
            Message::AddFiles => {
                let accepted = OpenFilePicker::new()
                    .title("Choose files to upload")
                    .filter_all()
                    .request_multiple(context, |result| match result {
                        Ok(paths) => Message::PathsPicked(paths),
                        Err(error) => Message::PickerFailed(error.to_string()),
                    });
                if !accepted {
                    self.upload_note = Some("The file picker could not be opened.".to_string());
                }
            }
            Message::AddFolders => {
                let accepted = FolderPicker::new()
                    .title("Choose folders to upload")
                    .request_multiple(context, |result| match result {
                        Ok(paths) => Message::PathsPicked(paths),
                        Err(error) => Message::PickerFailed(error.to_string()),
                    });
                if !accepted {
                    self.upload_note = Some("The folder picker could not be opened.".to_string());
                }
            }
            Message::PathsPicked(paths) => {
                for path in paths {
                    if !self.paths.contains(&path) {
                        self.paths.push(path);
                    }
                }
            }
            Message::PickerFailed(error) => {
                self.upload_note = Some(format!("Picker failed: {error}"));
            }
            Message::RemovePath(index) => {
                if index < self.paths.len() {
                    self.paths.remove(index);
                }
            }
            Message::ClearPaths => self.paths.clear(),
            Message::AlbumModeChanged(index) => {
                self.album_mode = AlbumMode::from_index(index);
            }
            Message::AlbumNameChanged(value) => self.album_name = value,
            Message::StartUpload => upload::start_upload(self, context),
            Message::CancelUpload => {
                if let Some(token) = &self.cancel_token {
                    token.cancel();
                }
                self.cancel_requested = true;
                self.upload_note = Some("Cancelling…".to_string());
            }

            // ---- Upload run (reporter bridge) ----
            Message::UploadStarted(total_files) => {
                self.running = true;
                self.total_files = total_files;
            }
            Message::UploadStopped => {
                self.running = false;
            }
            Message::UploadTotalBytes(total) => self.total_bytes = total,
            Message::UploadTotalBytesDelta(delta) => {
                let adjusted = self.total_bytes as i128 + delta as i128;
                self.total_bytes = adjusted.max(0) as u64;
            }
            Message::UploadWarning(warning) => {
                self.warnings.push(format!(
                    "{} — {}",
                    warning.file_path.display(),
                    warning.message
                ));
                if self.warnings.len() > 200 {
                    self.warnings.remove(0);
                }
            }
            Message::UploadThreadStatus(status) => {
                let row = WorkerRow {
                    worker: status.worker,
                    stage: upload::stage_label(&status.stage).to_string(),
                    file: upload::file_label(&status.file_path),
                    bytes_uploaded: status.bytes_uploaded,
                    bytes_total: status.bytes_total,
                    attempt: status.attempt,
                };
                match self
                    .workers
                    .iter_mut()
                    .find(|existing| existing.worker == row.worker)
                {
                    Some(existing) => *existing = row,
                    None => {
                        self.workers.push(row);
                        self.workers.sort_by_key(|existing| existing.worker);
                    }
                }
            }
            Message::UploadFileResult(result) => {
                let outcome = match result.outcome {
                    Outcome::Uploaded => "Uploaded",
                    Outcome::SkippedAlreadyPresent => "Skipped — already in library",
                    Outcome::SkippedUnsupported => "Skipped — unsupported file",
                    Outcome::Failed => "Failed",
                };
                self.results.push(ResultRow {
                    file: upload::file_label(&result.file_path),
                    outcome: outcome.to_string(),
                    message: result.message,
                });
                if self.results.len() > 1_000 {
                    self.results.remove(0);
                }
            }
            Message::UploadAlbumProgress(status) => {
                self.album_status = Some(format!(
                    "Album '{}': {} of {} added",
                    status.name, status.done, status.total
                ));
            }
            Message::UploadAlbumComplete(status) => {
                self.album_status = Some(format!(
                    "Album '{}' complete: {} of {} added",
                    status.name, status.done, status.total
                ));
            }
            Message::UploadAlbumError(name, message) => {
                self.album_status = Some(format!("Album '{name}' failed: {message}"));
            }
            Message::UploadRunEnded(note) => {
                self.running = false;
                self.cancel_token = None;
                self.upload_note = Some(if self.cancel_requested {
                    "Upload cancelled.".to_string()
                } else {
                    note
                });
                self.cancel_requested = false;
            }

            // ---- Accounts section ----
            Message::OAuthTokenChanged(value) => self.oauth_token = value,
            Message::RawCredentialChanged(value) => self.raw_credential = value,
            Message::OpenSignInPage => {
                // Dependency-free browser launch (Windows shell). The
                // sign-in itself happens in the browser, never in the app.
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", accounts::EMBEDDED_SETUP_URL])
                    .spawn();
                self.account_note = Some(
                    "Sign-in page opened in your browser. Copy the oauth_token cookie value \
                     from DevTools and paste it here."
                        .to_string(),
                );
            }
            Message::ConnectAccount => accounts::connect_account(self, context),
            Message::AccountConnected(Ok((email, credential, needs_token_binding))) => {
                self.auth_busy = false;
                self.oauth_token.clear();
                self.add_account(Account {
                    email,
                    credential,
                    needs_token_binding,
                });
            }
            Message::AccountConnected(Err(error)) => {
                self.auth_busy = false;
                self.account_note = Some(format!("Sign-in failed: {error}"));
            }
            Message::ImportRawCredential => {
                let raw = self.raw_credential.trim().to_string();
                match Credential::parse(&raw) {
                    Ok(credential) => {
                        let account = Account {
                            email: credential.email().to_string(),
                            credential: raw,
                            needs_token_binding: credential.needs_token_binding(),
                        };
                        self.raw_credential.clear();
                        self.add_account(account);
                    }
                    Err(error) => {
                        self.account_note =
                            Some(format!("That does not look like a credential: {error}"));
                    }
                }
            }
            Message::SetActiveAccount(email) => match &mut self.store {
                Some(service) => match store::set_active(service, &email) {
                    Ok(()) => {
                        self.refresh_from_store();
                        self.account_note = Some(format!("{email} is now the active account."));
                    }
                    Err(error) => {
                        self.account_note = Some(format!("Could not switch account: {error}"));
                    }
                },
                None => {
                    self.account_note =
                        Some("Config could not be loaded; accounts are unavailable.".to_string());
                }
            },
            Message::RemoveAccount(email) => match &mut self.store {
                Some(service) => match store::remove_account(service, &email) {
                    Ok(()) => {
                        self.refresh_from_store();
                        self.account_note = Some(format!("Removed account {email}."));
                    }
                    Err(error) => {
                        self.account_note = Some(format!("Could not remove account: {error}"));
                    }
                },
                None => {
                    self.account_note =
                        Some("Config could not be loaded; accounts are unavailable.".to_string());
                }
            },

            // ---- Settings section ----
            Message::PrefBoolChanged(field, value) => {
                field.apply(&mut self.prefs, value);
                self.persist_preferences();
            }
            Message::PrefThreadsChanged(value) => {
                if let Some(value) = value {
                    if value.is_finite() {
                        self.prefs.upload_threads = value.round().clamp(1.0, 16.0) as u32;
                        self.persist_preferences();
                    }
                }
            }
            Message::PrefProxyChanged(value) => {
                self.prefs.proxy = value;
                self.persist_preferences();
            }
            Message::PrefExcludePatternChanged(value) => {
                self.prefs.exclude_pattern = value;
                self.persist_preferences();
            }
        }
    }

    fn view(&self, _input: &(), context: &mut ViewContext<Self>) -> View {
        let section_view = match self.section {
            Section::Upload => upload::view(self, context),
            Section::Accounts => accounts::view(self, context),
            Section::Settings => settings::view(self, context),
        };
        let item = |tag: &'static str, label: &'static str, selected: bool| {
            KeyedView::new(
                tag,
                NavigationViewItem::new()
                    .tag(tag)
                    .is_selected(selected)
                    .content(label),
            )
        };
        let navigation = NavigationView::new()
            .pane_display_mode(NavigationViewPaneDisplayMode::Left)
            .pane_title("Outh")
            .is_settings_visible(false)
            .on_selected_tag_changed(context.callback(|tag: Option<Rc<str>>| {
                Message::NavigateTag(tag.map(|tag| tag.to_string()))
            }))
            .keyed_menu_items([
                item("upload", "Upload", self.section == Section::Upload),
                item("accounts", "Accounts", self.section == Section::Accounts),
            ])
            .keyed_footer_menu_items([item(
                "settings",
                "Settings",
                self.section == Section::Settings,
            )])
            .content(section_view);
        context.window_frame("Outh", navigation)
    }
}

fn main() {
    App::run_component::<OuthApp>(()).unwrap();
}
