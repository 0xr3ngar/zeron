//! Account identities share one privacy treatment, including email fallbacks
//! used in place of a display name.

use gpui::{AnyElement, App, Corners, SharedString, canvas, div, prelude::*, px};

use crate::{frost, settings, typography::ui_rems};

#[derive(Debug, PartialEq, Eq)]
enum IdentityLabel {
    Visible(SharedString),
    HiddenEmail,
}

fn label(text: SharedString, hide_emails: bool) -> IdentityLabel {
    let has_email = text
        .split_once('@')
        .is_some_and(|(local, domain)| !local.trim().is_empty() && !domain.trim().is_empty());
    if hide_emails && has_email {
        IdentityLabel::HiddenEmail
    } else {
        IdentityLabel::Visible(text)
    }
}

pub fn emails_hidden(cx: &App) -> bool {
    settings::current(cx).blur_emails
}

pub fn set_emails_hidden(hidden: bool, cx: &mut App) {
    if settings::update(settings::SavePolicy::Immediate, cx, |settings| {
        settings.blur_emails = hidden;
    }) {
        cx.refresh_windows();
    }
}

pub fn identity(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    if let IdentityLabel::Visible(text) = label(text.into(), emails_hidden(cx)) {
        return div().min_w_0().truncate().child(text).into_any_element();
    }

    // Never put the address beneath the effect: unsupported renderers,
    // accessibility, and transitions must not reveal the original text.
    div()
        .relative()
        .min_w_0()
        .w(ui_rems(128.0))
        .max_w_full()
        .overflow_hidden()
        .rounded(px(5.0))
        .child(div().truncate().opacity(0.5).child("hidden@email.com"))
        .child(frost::layered(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    window.paint_backdrop_blur(bounds, Corners::all(px(5.0)), px(3.0));
                },
            )
            .absolute()
            .inset_0(),
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_removes_addresses_but_keeps_names_and_github_handles() {
        for address in [
            "person@example.com",
            "Person <person@example.com>",
            "person@localhost",
        ] {
            assert_eq!(label(address.into(), true), IdentityLabel::HiddenEmail);
            assert_eq!(
                label(address.into(), false),
                IdentityLabel::Visible(address.into())
            );
        }
        for name in ["Person", "@person", "Unknown account"] {
            assert_eq!(
                label(name.into(), true),
                IdentityLabel::Visible(name.into())
            );
        }
    }
}
