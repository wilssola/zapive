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

// The "@query" being typed right before the cursor (byte offsets of the
// "@" and of the cursor), when the cursor sits inside one: the "@" must
// start a word and the query holds no whitespace.
pub fn mention_query(text: &str, cursor: usize) -> Option<(usize, usize)> {
    let cursor = cursor.min(text.len());
    let cursor = (0..=cursor).rev().find(|&i| text.is_char_boundary(i))?;
    let head = &text[..cursor];
    let at = head.rfind('@')?;
    let query = &head[at + 1..];
    if query.contains(char::is_whitespace) {
        return None;
    }
    if at > 0 && !head[..at].ends_with(char::is_whitespace) {
        return None;
    }
    Some((at, cursor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mention_query_finds_the_word_at_the_cursor() {
        assert_eq!(mention_query("@", 1), Some((0, 1)));
        assert_eq!(mention_query("oi @jo", 6), Some((3, 6)));
        assert_eq!(mention_query("oi @jo tudo", 6), Some((3, 6)));
        // Cursor past a space: no longer inside the mention.
        assert_eq!(mention_query("oi @jo tudo", 7), None);
        // An "@" glued to a word is an address, not a mention.
        assert_eq!(mention_query("mail@x", 6), None);
        // Multi-byte text before the cursor keeps byte offsets valid.
        assert_eq!(mention_query("ação @ma", 10), Some((7, 10)));
        assert_eq!(mention_query("", 0), None);
    }

    #[test]
    fn markdown_keeps_links_and_mentions() {
        let resolve = |num: &str| MentionTarget {
            name: Some("Ana".into()),
            jid: format!("{num}@s.whatsapp.net"),
        };
        let md = to_markdown("oi @5511999 veja https://x.io/a_b *ok*", &resolve);
        assert!(md.contains("[@Ana](5511999@s.whatsapp.net)"));
        assert!(md.contains("(https://x.io/a_b)"));
        assert!(md.ends_with("**ok**"));
    }
}
