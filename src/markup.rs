// WhatsApp's lightweight markup (*bold*, _italic_, ~strike~, ```mono```)
// translated into the markdown subset Slint's StyledText understands.
// Everything outside a marker is escaped so stray symbols stay literal.
// Port of src/markup.ts on master.
use regex::Regex;
use std::sync::OnceLock;

fn token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"```([\s\S]+?)```|`([^`\n]+?)`|\*([^*\n]+?)\*|_([^_\n]+?)_|~([^~\n]+?)~")
            .expect("valid token regex")
    })
}

fn mention_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)@(\d{5,20}|all|everyone)\b").expect("valid mention regex"))
}

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)https?://[^\s<>\]]+").expect("valid url regex"))
}

fn escape_md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '~' | '[' | ']' | '(' | ')' | '#' | '>' | '+' | '-' | '.' | '!' | '|') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub fn has_markup(text: &str) -> bool {
    token_re().is_match(text) || mention_re().is_match(text) || url_re().is_match(text)
}

// Where a mention points: the chat to open and the name to show, when
// the id belongs to someone we know.
pub struct MentionTarget {
    pub name: Option<String>,
    pub jid: String,
}

fn render_links(text: &str) -> String {
    let mut out = String::new();
    let mut last = 0;
    for m in url_re().find_iter(text) {
        out.push_str(&escape_md(&text[last..m.start()]));
        let url = m.as_str();
        out.push_str(&format!("[{}]({url})", escape_md(url)));
        last = m.end();
    }
    out.push_str(&escape_md(&text[last..]));
    out
}

// Mentions arrive as @<number>; show the contact name and link it to the
// conversation so clicking opens that chat.
fn render_mentions(text: &str, resolve: &dyn Fn(&str) -> MentionTarget) -> String {
    let mut out = String::new();
    let mut last = 0;
    for caps in mention_re().captures_iter(text) {
        let whole = caps.get(0).unwrap();
        out.push_str(&render_links(&text[last..whole.start()]));
        let token = caps.get(1).unwrap().as_str();
        if token.bytes().all(|b| b.is_ascii_digit()) {
            // A mention is always a link, named or not: an unknown id
            // still opens the conversation with that person.
            let hit = resolve(token);
            let label = hit.name.unwrap_or_else(|| token.to_string());
            out.push_str(&format!("[@{}]({})", escape_md(&label), hit.jid));
        } else {
            out.push_str(&format!("**@{}**", escape_md(token)));
        }
        last = whole.end();
    }
    out.push_str(&render_links(&text[last..]));
    out
}

pub fn to_markdown(text: &str, resolve: &dyn Fn(&str) -> MentionTarget) -> String {
    let plain = |part: &str| render_mentions(part, resolve);
    let mut out = String::new();
    let mut last = 0;
    for caps in token_re().captures_iter(text) {
        let whole = caps.get(0).unwrap();
        out.push_str(&plain(&text[last..whole.start()]));
        if let Some(code) = caps.get(1).or_else(|| caps.get(2)) {
            out.push_str(&format!("`{}`", code.as_str().replace('`', "")));
        } else if let Some(bold) = caps.get(3) {
            out.push_str(&format!("**{}**", plain(bold.as_str())));
        } else if let Some(italic) = caps.get(4) {
            out.push_str(&format!("*{}*", plain(italic.as_str())));
        } else if let Some(strike) = caps.get(5) {
            out.push_str(&format!("~~{}~~", escape_md(strike.as_str())));
        }
        last = whole.end();
    }
    // The tail is literal text as well: it still needs mentions and links.
    out.push_str(&plain(&text[last..]));
    out
}
