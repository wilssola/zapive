// The only module that talks to the operating system directly. Everything
// per-OS funnels through here so the rest of the app stays portable.
use std::path::PathBuf;

#[derive(Clone, Debug, Default)]
pub struct ToastPayload {
    pub token: Option<String>,
    pub thumbnail: Option<PathBuf>,
    pub play: bool,
    pub reply: bool,
}
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
#[cfg(all(unix, not(target_os = "macos")))]
pub use linux::*;

// What a toast click asked for, parsed from the activation arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToastAction {
    // Body click. Without a token the click only raises the app.
    Open(Option<String>),
    Reply(String),
    Play(String),
    // Malformed or unknown arguments: ignored, never acted on.
    Invalid,
}

// Notification tokens are 16 random bytes in lowercase hex.
const TOKEN_LEN: usize = 32;

fn valid_token(token: &str) -> bool {
    token.len() == TOKEN_LEN && token.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse_toast_args(arguments: &str) -> ToastAction {
    match arguments {
        "" | "open" | "default" => return ToastAction::Open(None),
        _ => {}
    }
    let Some((kind, token)) = arguments.split_once(':') else { return ToastAction::Invalid };
    if !valid_token(token) {
        return ToastAction::Invalid;
    }
    let token = token.to_string();
    match kind {
        "open" => ToastAction::Open(Some(token)),
        "reply" => ToastAction::Reply(token),
        "play" => ToastAction::Play(token),
        _ => ToastAction::Invalid,
    }
}

#[cfg(test)]
mod toast_args_tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn body_click_variants_open_without_a_chat() {
        for args in ["", "open", "default"] {
            assert_eq!(parse_toast_args(args), ToastAction::Open(None));
        }
    }

    #[test]
    fn parses_open_reply_and_play_with_a_token() {
        assert_eq!(parse_toast_args(&format!("open:{TOKEN}")), ToastAction::Open(Some(TOKEN.into())));
        assert_eq!(parse_toast_args(&format!("reply:{TOKEN}")), ToastAction::Reply(TOKEN.into()));
        assert_eq!(parse_toast_args(&format!("play:{TOKEN}")), ToastAction::Play(TOKEN.into()));
    }

    #[test]
    fn rejects_unknown_actions() {
        for kind in ["delete", "OPEN", "open ", "x", ""] {
            if kind.is_empty() {
                assert_eq!(parse_toast_args(&format!(":{TOKEN}")), ToastAction::Invalid);
            } else {
                assert_eq!(parse_toast_args(&format!("{kind}:{TOKEN}")), ToastAction::Invalid);
            }
        }
    }

    #[test]
    fn rejects_malformed_tokens() {
        let long = format!("{TOKEN}0");
        for token in ["", "abc", "ZZZZ456789abcdef0123456789abcdef", "0123456789ABCDEF0123456789ABCDEF", long.as_str(), "0123456789abcdef0123456789abcde:"] {
            for kind in ["open", "reply", "play"] {
                assert_eq!(parse_toast_args(&format!("{kind}:{token}")), ToastAction::Invalid, "{kind}:{token}");
            }
        }
    }

    #[test]
    fn rejects_input_without_a_separator_or_with_extra_parts() {
        for args in ["reply", "play", "garbage", "open:open", &format!("play:{TOKEN}:x"), &format!(" open:{TOKEN}")] {
            assert_eq!(parse_toast_args(args), ToastAction::Invalid, "{args}");
        }
    }
}
