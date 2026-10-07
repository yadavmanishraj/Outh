//! Accounts section: account list + Embedded Setup sign-in + raw
//! credential import.
//!
//! Sign-in follows gotohp's only Windows-native flow (googleauth.go):
//! the user signs in on Google's Embedded Setup page in their *browser*,
//! copies the `oauth_token` cookie value, pastes it here, and
//! `EmbeddedSetupAuth::exchange_oauth_token` trades it for a master-token
//! credential. This app never asks for a Google password.
//!
//! The exchange is a single request/response, so it uses reactor's
//! `spawn_background` exactly like the `async-state` sample: work runs off
//! the UI thread and the returned value arrives as one message.
//!
//! Layout (REDESIGN_SPEC §5): page title + note bar, one card per
//! account, a numbered sign-in stepper card, and the raw-credential
//! import demoted into a collapsed Expander. The store-load error is
//! NOT rendered here — the root view shows it above every page (§2).

use windows_reactor::*;

use outh_core::config::Account;

use crate::theme;
use crate::{Message, Note, NoteSeverity, OuthApp};

/// Embedded Setup page, opened in the system browser.
pub const EMBEDDED_SETUP_URL: &str = "https://accounts.google.com/EmbeddedSetup";

/// Handles Message::ConnectAccount (UI thread): kicks off the exchange in
/// the background; the result lands as Message::AccountConnected.
pub fn connect_account(app: &mut OuthApp, context: &ComponentContext<OuthApp>) {
    if app.auth_busy {
        return;
    }
    let token = app.oauth_token.trim().to_string();
    if token.is_empty() {
        app.account_note = Some(Note::warning("Paste the oauth_token cookie value first."));
        return;
    }
    app.auth_busy = true;
    app.account_note = Some(Note::info("Contacting Google…"));
    let proxy = app.prefs.proxy.clone();
    _ = context.spawn_background(move |_| {
        let auth = outh_core::auth::EmbeddedSetupAuth::new(&proxy);
        match auth.exchange_oauth_token(&token) {
            Ok(credential) => Message::AccountConnected(Ok((
                credential.email().to_string(),
                credential.to_string(),
                credential.needs_token_binding(),
            ))),
            // Map the core error to recovery copy HERE, while the variant
            // is still known (F-05) — by the time it is a String, "bad
            // authentication" and a dead network are indistinguishable.
            Err(error) => Message::AccountConnected(Err(sign_in_error_copy(&error))),
        }
    });
}

/// User-facing copy for a failed Embedded Setup exchange (spec §5.2).
/// Anything that is not one of the two known Google answers is treated
/// as a reachability problem, which is what it almost always is.
fn sign_in_error_copy(error: &outh_core::Error) -> String {
    match error {
        outh_core::Error::BadAuthentication => {
            "Google rejected that token — obtain a fresh oauth_token cookie and try again."
        }
        outh_core::Error::NeedsBrowser => {
            "Google wants a browser sign-in first — complete it on the Embedded \
             Setup page, then copy a fresh cookie."
        }
        _ => "Couldn't reach Google — check your connection and proxy settings, then try again.",
    }
    .to_string()
}

/// Avatar initials for an account: the first alphanumeric character of
/// the email, upper-cased ("person@example.com" → "P").
fn initials_for(email: &str) -> String {
    email
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default()
}

/// One account = one card (spec §5.1): avatar, email + status, actions.
/// "Set active" is omitted (not disabled) on the active account (V-22);
/// removal routes through the global confirmation (RequestRemoveAccount,
/// I-12). While an upload is running, account switching and removal are
/// disabled — the run keeps the account it started with (F-29).
fn account_card(app: &OuthApp, account: &Account, context: &mut ViewContext<OuthApp>) -> View {
    let sender = context.sender();
    let is_active = app.active_email.as_deref() == Some(account.email.as_str());
    let email = account.email.clone();
    let email_for_set = email.clone();

    let avatar = PersonPicture::new()
        .initials(initials_for(&email))
        .width(40.0)
        .height(40.0)
        .grid_column(0)
        .vertical_alignment(VerticalAlignment::Center);

    let mut middle: Vec<View> = vec![theme::strong(&email)];
    if is_active {
        middle.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(theme::SPACE_XS)
                .children(vec![
                    SymbolIcon::new()
                        .symbol(Symbol::Accept)
                        .width(theme::SPACE_M)
                        .height(theme::SPACE_M)
                        .vertical_alignment(VerticalAlignment::Center)
                        .into(),
                    theme::caption("Active"),
                ])
                .into(),
        );
    }
    let middle = StackPanel::new()
        .spacing(theme::SPACE_XS)
        .grid_column(1)
        .vertical_alignment(VerticalAlignment::Center)
        .children(middle);

    let mut actions: Vec<View> = Vec::new();
    if !is_active {
        let set_sender = sender.clone();
        actions.push(
            Button::new()
                .style(ButtonStyle::Subtle)
                .is_enabled(!app.running)
                .on_click(move || {
                    _ = set_sender.send(Message::SetActiveAccount(email_for_set.clone()));
                })
                .content("Set active")
                .into(),
        );
    }
    actions.push(
        Button::new()
            .style(ButtonStyle::Subtle)
            .is_enabled(!app.running)
            .on_click(move || {
                _ = sender.send(Message::RequestRemoveAccount(email.clone()));
            })
            .content("Remove")
            .into(),
    );
    let actions = StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(theme::SPACE_S)
        .grid_column(2)
        .vertical_alignment(VerticalAlignment::Center)
        .children(actions);

    let grid = Grid::new()
        .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
        .column_spacing(theme::SPACE_M)
        .children(vec![avatar.into(), middle.into(), actions.into()]);

    let mut card_children: Vec<View> = vec![grid.into()];
    if account.needs_token_binding {
        // Token binding is deferred in v1 (CONTRACT.md decision 4):
        // Embedded-Setup accounts never need it, but an imported
        // credential may. Badge it honestly instead of failing later.
        card_children.push(theme::info_bar(
            NoteSeverity::Warning,
            None,
            "Needs token binding, which this version doesn't support — uploads \
             from this account will fail. Accounts added through Embedded Setup \
             (below) don't need it.",
            None,
        ));
    }
    theme::card(card_children)
}

/// One numbered step of the sign-in stepper: Strong title + Body text.
fn step(title: &str, text: &str) -> View {
    StackPanel::new()
        .spacing(theme::SPACE_XS)
        .children(vec![theme::strong(title), theme::body(text)])
        .into()
}

/// The sign-in card (spec §5.2 / R-2): three numbered steps, then the
/// token field and actions. Enter in the token field submits — text
/// boxes have no key events in this stack, so a wrapping Border routes
/// the preview key (I-05, the canvas/keyboard sample pattern).
fn sign_in_card(app: &OuthApp, context: &mut ViewContext<OuthApp>) -> View {
    let key_sender = context.sender();
    let token_box = Border::new()
        .on_preview_key_down(RoutedCallback::new(move |info: KeyEventInfo| {
            if info.key == VirtualKey::ENTER {
                _ = key_sender.send(Message::ConnectAccount);
                true
            } else {
                false
            }
        }))
        .content(
            PasswordBox::new()
                .header("oauth_token cookie value")
                .password(&app.oauth_token)
                .is_enabled(!app.auth_busy)
                .on_password_changed(context.callback(|value: std::rc::Rc<str>| {
                    Message::OAuthTokenChanged(value.to_string())
                })),
        );

    let mut buttons: Vec<View> = vec![
        Button::new()
            .style(ButtonStyle::Default)
            .on_click(context.callback(|()| Message::OpenSignInPage))
            .content("Open sign-in page")
            .into(),
        Button::new()
            .style(ButtonStyle::Accent)
            .is_enabled(!app.auth_busy && !app.oauth_token.trim().is_empty())
            .on_click(context.callback(|()| Message::ConnectAccount))
            .content("Connect")
            .into(),
    ];
    if app.auth_busy {
        buttons.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(theme::SPACE_S)
                .children(vec![
                    ProgressRing::new()
                        .is_active(true)
                        .width(20.0)
                        .height(20.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .into(),
                    theme::secondary("Connecting…"),
                ])
                .into(),
        );
    }
    let buttons = StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(theme::SPACE_S)
        .children(buttons);

    theme::card(vec![
        theme::section_title("Add an account"),
        step(
            "1 — Sign in on Google's page",
            "Open the Embedded Setup page and sign in with the Google \
             account you want to connect.",
        ),
        step(
            "2 — Copy the oauth_token cookie",
            "In the browser, open DevTools (F12) → Application (Chrome/Edge) \
             or Storage (Firefox) → Cookies → https://accounts.google.com, \
             and copy the value of the oauth_token cookie.",
        ),
        step(
            "3 — Paste it here and connect",
            "Paste the value below and choose Connect. Outh trades it for a \
             long-lived sign-in, so you only do this once per account.",
        ),
        theme::body(
            "The account you sign in as in the browser is the account that \
             gets connected.",
        ),
        theme::caption("This app never asks for your Google password."),
        token_box.into(),
        buttons.into(),
    ])
}

/// Raw-credential import (spec §5.3 / V-25): demoted into a collapsed
/// Expander so it stops competing with the primary sign-in flow. The
/// Import button sends Message::ImportRawCredential.
fn import_expander(app: &OuthApp, context: &mut ViewContext<OuthApp>) -> View {
    Expander::new()
        .header("Advanced — import a raw credential")
        .is_expanded(false)
        .content(
            StackPanel::new()
                .spacing(theme::SPACE_S)
                .children(vec![
                    theme::body(
                        "Paste a credential string captured from a ReVanced/GmsCore \
                         or rooted-device Photos sign-in (the query-style auth \
                         request body). It is validated before it is stored: \
                         imports with missing fields, or for an email that \
                         already has an account, are rejected.",
                    ),
                    TextBox::new(app.raw_credential.clone())
                        .header("Raw credential string")
                        .accepts_return(true)
                        .text_wrapping(TextWrapping::Wrap)
                        .on_text_changed(context.callback(|value: std::rc::Rc<str>| {
                            Message::RawCredentialChanged(value.to_string())
                        }))
                        .into(),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(theme::SPACE_S)
                        .children(vec![
                            Button::new()
                                .style(ButtonStyle::Default)
                                .is_enabled(!app.raw_credential.trim().is_empty())
                                .on_click(context.callback(|()| Message::ImportRawCredential))
                                .content("Import credential")
                                .into(),
                        ])
                        .into(),
                ]),
        )
        .into()
}

pub fn view(app: &OuthApp, context: &mut ViewContext<OuthApp>) -> View {
    let mut children: Vec<View> = Vec::new();

    children.push(theme::page_title("Accounts"));
    // The page's feedback lives directly under the title (F-09); the
    // store-load error renders globally above the page (main.rs, §2).
    if let Some(note) = &app.account_note {
        children.push(theme::note_bar(
            note,
            context.message(Message::DismissAccountNote),
        ));
    }

    if app.accounts.is_empty() {
        children.push(theme::secondary("No accounts yet — add one below."));
    }
    for account in &app.accounts {
        children.push(account_card(app, account, context));
    }

    children.push(sign_in_card(app, context));
    children.push(import_expander(app, context));

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

/// Handles Message::ImportRawCredential: validated import through core's
/// `ConfigService::add_credentials` (required-field validation + duplicate
/// rejection), gated by `Credential::looks_like` so a wrong paste (e.g. a
/// bare oauth_token) is rejected with guidance instead of a core error.
pub fn import_raw_credential(app: &mut OuthApp) {
    let raw = app.raw_credential.trim().to_string();
    if !outh_core::credential::Credential::looks_like(&raw) {
        app.account_note = Some(Note::warning(
            "That doesn't look like a credential string — nothing was imported.",
        ));
        return;
    }
    match &mut app.store {
        Some(service) => match crate::store::add_credentials(service, &raw) {
            Ok(email) => {
                app.raw_credential.clear();
                app.refresh_from_store();
                app.account_note = Some(Note::success(format!("Imported account {email}.")));
            }
            Err(error) => {
                app.account_note = Some(Note::error(error.to_string()));
            }
        },
        None => {
            app.account_note = Some(Note::error(
                "Config could not be loaded, so the credential was not imported.",
            ));
        }
    }
}
