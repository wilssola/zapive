// WhatsApp's lightweight markup (*bold*, _italic_, ~strike~, ```mono```)
// translated into the markdown subset Slint's StyledText understands.
// Everything outside a marker is escaped so stray symbols stay literal.
// Port of src/markup.ts on master.
//
// The styled rendering and the plain one (the text a transparent
// TextInput lays over it for selection) come from the same token
// stream, so a marker either styles in both or is literal in both.
use regex::Regex;
use std::sync::OnceLock;

fn token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // A marker pair only counts when the text inside starts and ends
        // on something other than whitespace: "* a *" stays literal for
        // WhatsApp, and CommonMark would not style it either.
        Regex::new(concat!(
            r"```([\s\S]+?)```|`([^`\n]+?)`",
            r"|\*([^*\n\s](?:[^*\n]*?[^*\n\s])?)\*",
            r"|_([^_\n\s](?:[^_\n]*?[^_\n\s])?)_",
            r"|~([^~\n\s](?:[^~\n]*?[^~\n\s])?)~",
        ))
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

const NBSP: char = '\u{a0}';

fn escape_md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        // '<' and '&' too: otherwise "<b>" becomes an HTML tag and
        // "&amp;" an entity, and the styled text drifts from the plain.
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '~' | '[' | ']' | '(' | ')' | '#' | '>' | '+' | '-' | '.' | '!'
                | '|' | '<' | '&'
        ) {
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

enum Token<'a> {
    Plain(&'a str),
    Code(&'a str),
    Bold(&'a str),
    Italic(&'a str),
    Strike(&'a str),
}

// Marker pairs, with the literal text between them. An underscore pair
// glued to letters on either side ("snake_case_name") is literal: it is
// not a WhatsApp style and CommonMark would not style it either.
fn tokens(text: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut last = 0;
    for caps in token_re().captures_iter(text) {
        let whole = caps.get(0).unwrap();
        if let Some(italic) = caps.get(4) {
            let before = text[..whole.start()].chars().next_back();
            let after = text[whole.end()..].chars().next();
            if before.is_some_and(char::is_alphanumeric) || after.is_some_and(char::is_alphanumeric) {
                continue;
            }
            out.push(Token::Plain(&text[last..whole.start()]));
            out.push(Token::Italic(italic.as_str()));
        } else if let Some(code) = caps.get(1).or_else(|| caps.get(2)) {
            out.push(Token::Plain(&text[last..whole.start()]));
            out.push(Token::Code(code.as_str()));
        } else if let Some(bold) = caps.get(3) {
            out.push(Token::Plain(&text[last..whole.start()]));
            out.push(Token::Bold(bold.as_str()));
        } else if let Some(strike) = caps.get(5) {
            out.push(Token::Plain(&text[last..whole.start()]));
            out.push(Token::Strike(strike.as_str()));
        } else {
            continue;
        }
        last = whole.end();
    }
    out.push(Token::Plain(&text[last..]));
    out
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

// A code span keeps its text verbatim, one span per line: CommonMark
// folds a newline inside a span into a space, and trims one space off
// each end, and the plain rendering keeps both.
fn render_code(code: &str) -> String {
    code.replace('`', "")
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                return String::new();
            }
            let mut line = line.to_string();
            if line.starts_with(' ') {
                line.replace_range(..1, "\u{a0}");
            }
            if line.ends_with(' ') {
                let at = line.len() - 1;
                line.replace_range(at.., "\u{a0}");
            }
            format!("`{line}`")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// Every line of the message must stay a line of the rendering: CommonMark
// drops blank lines (they only end a paragraph) and the indentation of
// continuation lines, and reads four leading spaces as a code block. A
// no-break space is not whitespace to it, so it keeps blank lines and
// indentation on screen where the plain text has them.
fn keep_lines(md: &str) -> String {
    md.split('\n')
        .map(|line| {
            let indent = line.len() - line.trim_start_matches(' ').len();
            let rest = &line[indent..];
            if rest.trim().is_empty() {
                return NBSP.to_string();
            }
            let mut out = String::with_capacity(line.len() + indent);
            for _ in 0..indent {
                out.push(NBSP);
            }
            out.push_str(rest);
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn to_markdown(text: &str, resolve: &dyn Fn(&str) -> MentionTarget) -> String {
    let plain = |part: &str| render_mentions(part, resolve);
    let mut out = String::new();
    for token in tokens(text) {
        match token {
            Token::Plain(part) => out.push_str(&plain(part)),
            Token::Code(code) => out.push_str(&render_code(code)),
            Token::Bold(inner) => out.push_str(&format!("**{}**", plain(inner))),
            Token::Italic(inner) => out.push_str(&format!("*{}*", plain(inner))),
            Token::Strike(inner) => out.push_str(&format!("~~{}~~", escape_md(inner))),
        }
    }
    keep_lines(&out)
}

// What StyledText ends up drawing, as plain text, plus the byte ranges
// (in that text) that are links or mentions and where they lead. This
// is the text a transparent TextInput lays over the styled one so the
// glyphs line up, and the map a click on it is resolved against.
pub struct PlainRender {
    pub text: String,
    pub spans: Vec<(usize, usize, String)>,
}

pub fn render_plain(text: &str, resolve: &dyn Fn(&str) -> MentionTarget) -> PlainRender {
    let mut out = PlainRender { text: String::with_capacity(text.len()), spans: Vec::new() };
    for token in tokens(text) {
        match token {
            Token::Plain(part) | Token::Bold(part) | Token::Italic(part) => {
                plain_mentions(part, resolve, &mut out)
            }
            Token::Code(code) => out.text.push_str(&code.replace('`', "")),
            Token::Strike(inner) => out.text.push_str(inner),
        }
    }
    out
}

fn plain_mentions(text: &str, resolve: &dyn Fn(&str) -> MentionTarget, out: &mut PlainRender) {
    let mut last = 0;
    for caps in mention_re().captures_iter(text) {
        let whole = caps.get(0).unwrap();
        plain_links(&text[last..whole.start()], out);
        let token = caps.get(1).unwrap().as_str();
        if token.bytes().all(|b| b.is_ascii_digit()) {
            let hit = resolve(token);
            let label = format!("@{}", hit.name.unwrap_or_else(|| token.to_string()));
            let start = out.text.len();
            out.text.push_str(&label);
            out.spans.push((start, out.text.len(), hit.jid));
        } else {
            out.text.push_str(whole.as_str());
        }
        last = whole.end();
    }
    plain_links(&text[last..], out);
}

fn plain_links(text: &str, out: &mut PlainRender) {
    let mut last = 0;
    for m in url_re().find_iter(text) {
        out.text.push_str(&text[last..m.start()]);
        let start = out.text.len();
        out.text.push_str(m.as_str());
        out.spans.push((start, out.text.len(), m.as_str().to_string()));
        last = m.end();
    }
    out.text.push_str(&text[last..]);
}

// The link or mention under a byte offset of the plain rendering.
pub fn target_at(text: &str, offset: usize, resolve: &dyn Fn(&str) -> MentionTarget) -> Option<String> {
    render_plain(text, resolve)
        .spans
        .into_iter()
        .find(|(start, end, _)| offset >= *start && offset < *end)
        .map(|(_, _, target)| target)
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

    fn ana(num: &str) -> MentionTarget {
        MentionTarget { name: Some("Ana".into()), jid: format!("{num}@s.whatsapp.net") }
    }

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
    fn plain_rendering_matches_the_styled_text() {
        let r = render_plain("oi @5511999 veja *https://x.io/a* fim", &ana);
        assert_eq!(r.text, "oi @Ana veja https://x.io/a fim");
        assert_eq!(r.spans[0], (3, 7, "5511999@s.whatsapp.net".to_string()));
        assert_eq!(r.spans[1], (13, 27, "https://x.io/a".to_string()));
        assert_eq!(target_at("oi @5511999 veja *https://x.io/a* fim", 20, &ana).as_deref(), Some("https://x.io/a"));
        assert_eq!(target_at("oi @5511999 veja *https://x.io/a* fim", 9, &ana), None);
    }

    #[test]
    fn markdown_keeps_links_and_mentions() {
        let md = to_markdown("oi @5511999 veja https://x.io/a_b *ok*", &ana);
        assert!(md.contains("[@Ana](5511999@s.whatsapp.net)"));
        assert!(md.contains("(https://x.io/a_b)"));
        assert!(md.ends_with("**ok**"));
    }

    // Blank lines and indentation would vanish in CommonMark and the
    // plain overlay would sit one line lower than the styled text.
    #[test]
    fn markdown_keeps_every_line() {
        let md = to_markdown("*a*\n\n  b\n    c", &ana);
        assert_eq!(md, "**a**\n\u{a0}\n\u{a0}\u{a0}b\n\u{a0}\u{a0}\u{a0}\u{a0}c");
        assert_eq!(md.split('\n').count(), render_plain("*a*\n\n  b\n    c", &ana).text.split('\n').count());
    }

    // Markers that WhatsApp leaves literal stay literal in both renderings.
    #[test]
    fn loose_markers_are_literal_in_both() {
        assert_eq!(render_plain("2 * 3 * 4", &ana).text, "2 * 3 * 4");
        assert_eq!(to_markdown("2 * 3 * 4", &ana), "2 \\* 3 \\* 4");
        assert_eq!(render_plain("snake_case_name", &ana).text, "snake_case_name");
        assert_eq!(to_markdown("snake_case_name", &ana), "snake\\_case\\_name");
        assert_eq!(render_plain("_it_ ok", &ana).text, "it ok");
        assert_eq!(to_markdown("_it_ ok", &ana), "*it* ok");
    }

    #[test]
    fn html_looking_text_stays_literal() {
        assert_eq!(to_markdown("a <b> & c", &ana), "a \\<b\\> \\& c");
    }

    #[test]
    fn code_spans_keep_their_lines() {
        assert_eq!(to_markdown("```x\n y```", &ana), "`x`\n`\u{a0}y`");
        assert_eq!(render_plain("```x\n y```", &ana).text, "x\n y");
    }
}
