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

use windows_reactor::*;

use crate::{Message, OuthApp};

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
        app.account_note = Some("Paste the oauth_token cookie value first.".to_string());
        return;
    }
    app.auth_busy = true;
    app.account_note = Some("Contacting Google…".to_string());
    _ = context.spawn_background(move |_| {
        let auth = outh_core::auth::EmbeddedSetupAuth::new();
        match auth.exchange_oauth_token(&token) {
            Ok(credential) => Message::AccountConnected(Ok((
                credential.email.clone(),
                credential.to_string(),
                credential.needs_token_binding(),
            ))),
            Err(error) => Message::AccountConnected(Err(error.to_string())),
        }
    });
}

pub fn view(app: &OuthApp, context: &mut ViewContext<OuthApp>) -> View {
    let sender = context.sender();
    let mut children: Vec<View> = Vec::new();

    children.push(TextBlock::new().text("Accounts").font_size(20.0).into());

    if let Some(error) = &app.store_error {
        children.push(
            TextBlock::new()
                .text(error.clone())
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        );
    }

    if app.accounts.is_empty() {
        children.push(
            TextBlock::new()
                .text("No accounts yet — add one below.")
                .opacity(0.6)
                .into(),
        );
    }
    for account in &app.accounts {
        let is_active = app.active_email.as_deref() == Some(account.email.as_str());
        let email = account.email.clone();
        let set_sender = sender.clone();
        let remove_sender = sender.clone();
        let mut line = email.clone();
        if is_active {
            line.push_str(" — active");
        }
        children.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    TextBlock::new().text(line),
                    Button::new()
                        .is_enabled(!is_active)
                        .on_click(move || {
                            _ = set_sender.send(Message::SetActiveAccount(email.clone()));
                        })
                        .content("Set active"),
                    Button::new()
                        .on_click(move || {
                            _ = remove_sender.send(Message::RemoveAccount(email.clone()));
                        })
                        .content("Remove"),
                ))
                .into(),
        );
        if account.needs_token_binding {
            // Token binding is deferred in v1 (CONTRACT.md decision 4):
            // Embedded-Setup accounts never need it, but an imported
            // credential may. Badge it honestly instead of failing later.
            children.push(
                TextBlock::new()
                    .text(
                        "This account needs token binding — unsupported in this version. \
                         Uploads with it will fail until token binding is implemented; \
                         sign-ins through Embedded Setup (below) do not need it.",
                    )
                    .text_wrapping(TextWrapping::Wrap)
                    .opacity(0.8)
                    .into(),
            );
        }
    }

    children.push(
        TextBlock::new()
            .text("Add an account")
            .font_size(16.0)
            .into(),
    );
    children.push(
        TextBlock::new()
            .text(
                "1. Open Google's Embedded Setup page and sign in there.\n\
                 2. In the browser, open DevTools → Application → Cookies for \
                 accounts.google.com and copy the value of the oauth_token cookie.\n\
                 3. Paste it below and choose Connect.\n\
                 This app never asks for your Google password.",
            )
            .text_wrapping(TextWrapping::Wrap)
            .opacity(0.8)
            .into(),
    );
    children.push(
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                Button::new()
                    .on_click(context.callback(|()| Message::OpenSignInPage))
                    .content("Open sign-in page"),
                PasswordBox::new()
                    .password(&app.oauth_token)
                    .placeholder_text("oauth_token cookie value")
                    .is_enabled(!app.auth_busy)
                    .on_password_changed(context.callback(|value: std::rc::Rc<str>| {
                        Message::OAuthTokenChanged(value.to_string())
                    })),
                Button::new()
                    .is_enabled(!app.auth_busy && !app.oauth_token.trim().is_empty())
                    .on_click(context.callback(|()| Message::ConnectAccount))
                    .content(if app.auth_busy {
                        "Connecting…"
                    } else {
                        "Connect"
                    }),
            ))
            .into(),
    );

    children.push(
        TextBlock::new()
            .text("Advanced — import a raw credential")
            .font_size(16.0)
            .into(),
    );
    children.push(
        TextBlock::new()
            .text(
                "Paste a credential string captured from a ReVanced/GmsCore or rooted-device \
                 Photos sign-in (the query-style auth request body). It is validated locally \
                 before it is stored.",
            )
            .text_wrapping(TextWrapping::Wrap)
            .opacity(0.8)
            .into(),
    );
    children.push(
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                PasswordBox::new()
                    .password(&app.raw_credential)
                    .placeholder_text("Raw credential string")
                    .on_password_changed(context.callback(|value: std::rc::Rc<str>| {
                        Message::RawCredentialChanged(value.to_string())
                    })),
                Button::new()
                    .is_enabled(!app.raw_credential.trim().is_empty())
                    .on_click(context.callback(|()| Message::ImportRawCredential))
                    .content("Import credential"),
            ))
            .into(),
    );

    if let Some(note) = &app.account_note {
        children.push(
            TextBlock::new()
                .text(note.clone())
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        );
    }

    ScrollViewer::new()
        .content(
            Border::new()
                .padding(Thickness::new(24.0, 24.0, 24.0, 24.0))
                .content(StackPanel::new().spacing(10.0).children(children)),
        )
        .into()
}
