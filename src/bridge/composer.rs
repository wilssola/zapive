// The composer: drafts, sending text, mentions and replies.
use super::*;

#[derive(Debug, PartialEq)]
struct MentionSpan {
    start: usize,
    end: usize,
    mention: usize,
}

fn byte_offset(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn token_word(c: char) -> bool {
    c.is_alphanumeric()
        || c == '_'
        || c == '@'
        || matches!(c, '\u{0300}'..='\u{036f}' | '\u{1ab0}'..='\u{1aff}' | '\u{1dc0}'..='\u{1dff}' | '\u{20d0}'..='\u{20ff}' | '\u{fe20}'..='\u{fe2f}')
}

fn mention_spans(text: &str, mentions: &[(String, String)]) -> Vec<MentionSpan> {
    let mut spans = Vec::new();
    let mut until = 0;
    for (start, c) in text.char_indices() {
        if c != '@' || start < until || text[..start].chars().next_back().is_some_and(token_word) {
            continue;
        }
        let best = mentions
            .iter()
            .enumerate()
            .filter_map(|(mention, (label, _))| {
                if label.is_empty() || !text[start + 1..].starts_with(label) {
                    return None;
                }
                let label_end = start + 1 + label.len();
                let end = label_end
                    + if text[label_end..].starts_with("\u{00a0}\u{00a0}\u{00a0}\u{00a0}\u{00a0}") {
                        10
                    } else if text[label_end..].starts_with("\u{00a0}\u{00a0}\u{00a0}") {
                        6
                    } else {
                        0
                    };
                if text[label_end..].chars().next().is_some_and(token_word) {
                    return None;
                }
                Some(MentionSpan {
                    start: if end > label_end && text[..start].ends_with('\u{00a0}') {
                        start - 2
                    } else {
                        start
                    },
                    end,
                    mention,
                })
            })
            .max_by_key(|span| span.end);
        if let Some(span) = best {
            until = span.end;
            spans.push(span);
        }
    }
    spans
}

fn expand_selection(spans: &[MentionSpan], start: usize, end: usize) -> (usize, usize) {
    let mut range = (start.min(end), start.max(end));
    for span in spans {
        if range.0 < span.end && range.1 > span.start {
            range.0 = range.0.min(span.start);
            range.1 = range.1.max(span.end);
        }
    }
    range
}

fn atomic_edit(old: &str, next: &str, mentions: &[(String, String)]) -> Option<(String, usize)> {
    let mut start = old
        .bytes()
        .zip(next.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    start = byte_offset(old, start);
    start = byte_offset(next, start);
    let mut suffix = old[start..]
        .bytes()
        .rev()
        .zip(next[start..].bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !next.is_char_boundary(next.len() - suffix) {
        suffix -= 1;
    }
    let end = old.len() - suffix;
    let (expanded_start, expanded_end) =
        expand_selection(&mention_spans(old, mentions), start, end);
    if (start, end) == (expanded_start, expanded_end) {
        return None;
    }
    let replacement = &next[start..next.len() - suffix];
    Some((
        format!(
            "{}{}{}",
            &old[..expanded_start],
            replacement,
            &old[expanded_end..]
        ),
        expanded_start + replacement.len(),
    ))
}

pub(super) fn remove_mention(
    text: &str,
    mentions: &[(String, String)],
    start: usize,
    end: usize,
) -> Option<String> {
    mention_spans(text, mentions)
        .iter()
        .any(|span| span.start == start && span.end == end)
        .then(|| format!("{}{}", &text[..start], &text[end..]))
}

fn send_mentions(
    text: &str,
    picked: &[(String, String)],
    jid: &str,
) -> (String, Vec<String>, bool) {
    let mut body = text.to_string();
    let mut mentions = Vec::new();
    let mut mention_all = false;
    for span in mention_spans(text, picked).into_iter().rev() {
        let target = &picked[span.mention].1;
        let target = if target == "all" {
            mention_all = true;
            jid
        } else {
            if !mentions.contains(target) {
                mentions.push(target.clone());
            }
            target.as_str()
        };
        let user = target.split('@').next().unwrap_or("");
        body.replace_range(span.start..span.end, &format!("@{user}"));
    }
    (body.trim().to_string(), mentions, mention_all)
}

fn mention_navigation(
    spans: &[MentionSpan],
    cursor: usize,
    anchor: usize,
    left: bool,
    shift: bool,
) -> Option<(usize, usize)> {
    let next = if !shift && cursor != anchor {
        let edge = if left {
            cursor.min(anchor)
        } else {
            cursor.max(anchor)
        };
        spans
            .iter()
            .find(|span| span.start < edge && edge < span.end)
            .map(|span| if left { span.start } else { span.end })
            .unwrap_or(edge)
    } else {
        let span = spans.iter().find(|span| {
            if left {
                span.start < cursor && cursor <= span.end
            } else {
                span.start <= cursor && cursor < span.end
            }
        })?;
        if left { span.start } else { span.end }
    };
    let anchor = if shift {
        spans
            .iter()
            .find(|span| span.start < anchor && anchor < span.end)
            .map(|span| if left { span.end } else { span.start })
            .unwrap_or(anchor)
    } else {
        next
    };
    Some((anchor, next))
}

pub(super) fn composer_key(
    ui: &AppWindow,
    key: SharedString,
    cursor: i32,
    anchor: i32,
    shift: bool,
    control: bool,
) -> bool {
    use slint::platform::Key;
    if control {
        return false;
    }
    let backspace = key == SharedString::from(Key::Backspace);
    let delete = key == SharedString::from(Key::Delete);
    let left = key == SharedString::from(Key::LeftArrow);
    let right = key == SharedString::from(Key::RightArrow);
    if !backspace && !delete && !left && !right {
        return false;
    }
    let text = ui.get_composer_text().to_string();
    let cursor = byte_offset(&text, cursor.max(0) as usize);
    let anchor = byte_offset(&text, anchor.max(0) as usize);
    let mentions: Vec<_> = ui
        .get_composer_mentions()
        .iter()
        .map(|span| (span.label.to_string(), String::new()))
        .collect();
    let spans = mention_spans(&text, &mentions);
    if backspace || delete {
        let range = if cursor != anchor {
            let range = expand_selection(&spans, cursor, anchor);
            if range == (cursor.min(anchor), cursor.max(anchor))
                && !spans.iter().any(|s| range.0 <= s.start && range.1 >= s.end)
            {
                return false;
            }
            range
        } else if let Some(span) = spans.iter().find(|span| {
            if backspace {
                span.start < cursor && cursor <= span.end
            } else {
                span.start <= cursor && cursor < span.end
            }
        }) {
            (span.start, span.end)
        } else {
            return false;
        };
        let next = format!("{}{}", &text[..range.0], &text[range.1..]);
        ui.invoke_set_composer(next.as_str().into(), range.0 as i32);
        defer(move |b| {
            if b.ui.get_composer_text().as_str() == next {
                b.composer_changed(&next, range.0);
            }
        });
        return true;
    }
    let Some((anchor, next)) = mention_navigation(&spans, cursor, anchor, left, shift) else {
        return false;
    };
    ui.invoke_select_composer(anchor as i32, next as i32);
    true
}

impl Bridge {
    pub(super) fn refresh_composer_mentions(&mut self, text: Option<&str>) {
        let current;
        let text = match text {
            Some(text) => text,
            None => {
                current = self.ui.get_composer_text().to_string();
                &current
            }
        };
        let rows: Vec<crate::ComposerMention> = mention_spans(text, &self.mentions)
            .into_iter()
            .map(|span| crate::ComposerMention {
                start: span.start as i32,
                end: span.end as i32,
                label: self.mentions[span.mention].0.as_str().into(),
            })
            .collect();
        self.ui
            .set_composer_mentions(ModelRc::new(VecModel::from(rows)));
    }

    pub(super) fn composer_cursor(&mut self, cursor: i32, anchor: i32) {
        let text = self.ui.get_composer_text().to_string();
        let cursor = byte_offset(&text, cursor.max(0) as usize);
        let anchor = byte_offset(&text, anchor.max(0) as usize);
        let spans = mention_spans(&text, &self.mentions);
        let snap = |offset: usize| {
            spans
                .iter()
                .find(|span| span.start < offset && offset < span.end)
                .map(|span| {
                    if offset - span.start < span.end - offset {
                        span.start
                    } else {
                        span.end
                    }
                })
                .unwrap_or(offset)
        };
        let (next_anchor, next_cursor) = if cursor == anchor {
            (snap(anchor), snap(cursor))
        } else {
            let (start, end) = expand_selection(&spans, cursor, anchor);
            if cursor < anchor {
                (end, start)
            } else {
                (start, end)
            }
        };
        if (next_anchor, next_cursor) != (anchor, cursor) {
            self.ui
                .invoke_select_composer(next_anchor as i32, next_cursor as i32);
        }
    }

    // ---- composing ----

    pub(super) fn save_draft(&mut self, text: &str, promote: bool) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        if text.is_empty() {
            if self.store.drafts.remove(&jid).is_some() {
                self.schedule_refresh_chats();
            }
            return;
        }
        if !self.store.chats.contains_key(&jid) {
            return;
        }
        let previous = self
            .store
            .drafts
            .get(&jid)
            .map_or(0, |draft| draft.promoted);
        let promoted = if promote && !text.trim().is_empty() {
            self.draft_sequence = self.draft_sequence.max(
                self.store
                    .drafts
                    .values()
                    .map(|draft| draft.promoted)
                    .max()
                    .unwrap_or(0),
            ) + 1;
            self.draft_sequence
        } else if text.trim().is_empty() {
            0
        } else {
            previous
        };
        self.store.drafts.insert(
            jid,
            crate::store::Draft {
                text: text.to_string(),
                mentions: self.mentions.clone(),
                promoted,
            },
        );
        self.schedule_refresh_chats();
    }

    pub(super) fn save_composer(&mut self, promote: bool) {
        let text = self.ui.get_composer_text().to_string();
        self.save_draft(&text, promote);
    }

    pub(super) fn handle_send_text(&mut self, text: &str) {
        let body = text.trim();
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        if body.is_empty() {
            return;
        }
        if !self
            .store
            .chats
            .get(&jid)
            .map(|c| c.can_send())
            .unwrap_or(true)
        {
            return;
        }
        let quote = self.reply_to.clone().and_then(|(chat, id)| {
            let m = self.store.messages_for(&chat).iter().find(|m| m.id == id)?;
            let raw = m.raw.clone()?;
            Some(QuoteRef {
                id: m.id.clone(),
                chat_jid: m.jid.clone(),
                sender_jid: if m.from_me {
                    self.self_jid.clone()
                } else {
                    m.sender_jid.clone()
                },
                message: raw,
            })
        });
        // "@Name" in the composer goes out as "@<number>" plus the jid in
        // the context, which is how every client renders a mention.
        let (body, mentions, mention_all) = send_mentions(text, &self.mentions, &jid);
        let mention_all = mention_all.then(|| self.store.chat_name(&jid));
        let message = crate::wa::build_text(&jid, body, &quote, mentions, mention_all);
        if self.enqueue_prepared(&jid, message, None) {
            self.store.drafts.remove(&jid);
            self.mentions.clear();
            self.clear_reply();
            self.ui.set_mention_open(false);
            self.ui.invoke_set_composer("".into(), 0);
            self.refresh_composer_mentions(Some(""));
            self.schedule_refresh_chats();
        }
    }

    // ---- mentions ----

    // Called on every composer edit with the cursor's byte offset: an
    // "@word" right before the cursor opens the member list.
    pub(super) fn composer_changed(&mut self, text: &str, cursor: usize) {
        let corrected = self
            .current_jid
            .as_ref()
            .and_then(|jid| self.store.drafts.get(jid))
            .and_then(|draft| atomic_edit(&draft.text, text, &self.mentions));
        let (text, cursor) = if let Some((ref next, cursor)) = corrected {
            self.ui
                .invoke_set_composer(next.as_str().into(), cursor as i32);
            (next.as_str(), cursor)
        } else {
            (text, byte_offset(text, cursor))
        };
        self.refresh_composer_mentions(Some(text));
        self.save_draft(text, false);
        self.mention_at = None;
        self.ui.set_mention_open(false);
        if mention_spans(text, &self.mentions)
            .iter()
            .any(|span| cursor > span.start && cursor <= span.end)
        {
            return;
        }
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        if !is_group(&jid) {
            return;
        }
        let Some((at, cursor)) = crate::markup::mention_query(text, cursor) else {
            self.mention_at = None;
            self.ui.set_mention_open(false);
            return;
        };
        let query = crate::search::fold(&text[at + 1..cursor]);
        self.mention_at = Some((at, cursor));
        let members: Vec<Member> = self.store.members_of(&jid).to_vec();
        self.ui.set_mention_member_count(members.len() as i32);
        let mut rows: Vec<MemberItem> = Vec::new();
        let all_label = t("mention.all");
        if query.is_empty() || crate::search::fold(&all_label).starts_with(&query) {
            rows.push(MemberItem {
                jid: "all".into(),
                name: all_label.into(),
                number: t("mention.all_hint").into(),
                hasAvatar: false,
                avatar: empty_image(),
                initial: "@".into(),
                colorIdx: 0,
                checked: false,
                admin: false,
            });
        }
        for m in members {
            let canon = self.store.canon_owned(&m.jid);
            if self.store.self_jids.contains(&canon) {
                continue;
            }
            let name = self.store.chat_name(&canon);
            let number = if m.phone.is_empty() {
                format_number(&canon)
            } else {
                format_number(&m.phone)
            };
            if !query.is_empty()
                && !crate::search::fold(&name).contains(&query)
                && !number.replace(' ', "").contains(&query)
            {
                continue;
            }
            rows.push(self.to_member_row(&canon, &name, &number, m.admin, false));
            if rows.len() >= 8 {
                break;
            }
        }
        if rows.is_empty() {
            self.ui.set_mention_open(false);
            return;
        }
        for row in &rows {
            if row.jid != "all" {
                self.queue_avatar(row.jid.as_str());
            }
        }
        self.mention_model.set_vec(rows);
        self.ui.set_mention_index(0);
        self.ui.set_mention_open(true);
    }

    pub(super) fn mention_pick(&mut self, idx: usize) {
        let Some(row) = self.mention_model.row_data(idx) else {
            return;
        };
        let Some((at, end)) = self.mention_at.take() else {
            return;
        };
        self.ui.set_mention_open(false);
        let text = self.ui.get_composer_text().to_string();
        if at > end || !text.is_char_boundary(at) || !text.is_char_boundary(end) {
            return;
        }
        let label = if row.jid == "all" {
            t("mention.all")
        } else {
            row.name.to_string()
        };
        let base = clean_text(&label).replace('@', "").replace(' ', "\u{00a0}");
        if base.is_empty() {
            return;
        }
        let mut label = self
            .mentions
            .iter()
            .find(|(_, jid)| jid == row.jid.as_str())
            .map(|(label, _)| label.clone())
            .unwrap_or_else(|| base.clone());
        if self
            .mentions
            .iter()
            .any(|(l, jid)| l == &label && jid != row.jid.as_str())
        {
            let number = clean_text(row.number.as_str()).replace('@', "");
            label = format!("{base} ({number})");
            let mut suffix = 2;
            while self
                .mentions
                .iter()
                .any(|(l, jid)| l == &label && jid != row.jid.as_str())
            {
                label = format!("{base} ({number} {suffix})");
                suffix += 1;
            }
        }
        let insert = format!("\u{00a0}@{label}\u{00a0}\u{00a0}\u{00a0}\u{00a0}\u{00a0} ");
        let next = format!("{}{}{}", &text[..at], insert, &text[end..]);
        let cursor = (at + insert.len()) as i32;
        self.mentions.retain(|(l, _)| l != &label);
        self.mentions.push((label, row.jid.to_string()));
        self.refresh_composer_mentions(Some(&next));
        self.save_draft(&next, false);
        self.ui.invoke_set_composer(next.into(), cursor);
    }

    pub(super) fn to_member_row(
        &self,
        jid: &str,
        name: &str,
        number: &str,
        admin: bool,
        checked: bool,
    ) -> MemberItem {
        let avatar = self.avatar_for(jid);
        MemberItem {
            jid: jid.into(),
            name: name.into(),
            number: number.into(),
            hasAvatar: avatar.is_some(),
            avatar: avatar.unwrap_or_else(empty_image),
            initial: initial_of(name).into(),
            colorIdx: color_idx_of(jid),
            checked,
            admin,
        }
    }

    pub(super) fn start_reply(&mut self, id: &str) {
        let Some(m) = self.find_message(id) else {
            return;
        };
        if m.raw.is_none() {
            return;
        }
        let name = if m.from_me {
            t("reactions.you")
        } else if m.sender.is_empty() {
            display_id(&m.jid)
        } else {
            m.sender.clone()
        };
        let preview = preview_body(m);
        self.reply_to = Some((m.jid.clone(), id.to_string()));
        self.ui.set_reply_open(true);
        self.ui.set_reply_name(name.into());
        self.ui.set_reply_text(preview.into());
        self.ui.invoke_focus_composer();
    }

    pub(super) fn clear_reply(&mut self) {
        self.reply_to = None;
        self.ui.set_reply_open(false);
        self.ui.set_reply_name("".into());
        self.ui.set_reply_text("".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mentions() -> Vec<(String, String)> {
        vec![
            ("Ana".into(), "1@s.whatsapp.net".into()),
            ("Ana Maria".into(), "2@s.whatsapp.net".into()),
            ("João".into(), "3@s.whatsapp.net".into()),
        ]
    }

    fn padded(label: &str) -> String {
        format!("\u{00a0}@{label}\u{00a0}\u{00a0}\u{00a0}")
    }

    #[test]
    fn padded_spans_include_padding_and_allow_adjacent_words() {
        let token = padded("Ana");
        let text = format!("ação{token}fim");
        let spans = mention_spans(&text, &mentions());
        assert_eq!(
            spans,
            vec![MentionSpan {
                start: "ação".len(),
                end: "ação".len() + token.len(),
                mention: 0,
            }]
        );
        let picked = vec![("Ana\u{00a0}Maria".into(), "2@s.whatsapp.net".into())];
        let text = padded("Ana\u{00a0}Maria");
        assert_eq!(mention_spans(&text, &picked)[0].end, text.len());
    }

    #[test]
    fn send_converts_padded_mentions_before_trimming_and_deduplicates() {
        let text = format!("  {} {} {} ", padded("Ana"), padded("João"), padded("Ana"));
        let (body, targets, all) = send_mentions(&text, &mentions(), "9@g.us");
        assert_eq!(body, "@1 @3 @1");
        assert_eq!(targets, vec!["1@s.whatsapp.net", "3@s.whatsapp.net"]);
        assert!(!all);
        assert_eq!(send_mentions(&padded("Ana"), &mentions(), "9@g.us").0, "@1");
        let picked = vec![("Todos".into(), "all".into())];
        assert_eq!(
            send_mentions(&padded("Todos"), &picked, "9@g.us"),
            ("@9".into(), vec![], true)
        );
    }

    #[test]
    fn padded_mentions_are_atomic_for_edits_and_remove() {
        let token = padded("Ana");
        let old = format!("oi {token} fim");
        let start = 3;
        let end = start + token.len();
        assert_eq!(
            remove_mention(&old, &mentions(), start, end),
            Some("oi  fim".into())
        );
        for (offset, c) in token.char_indices() {
            let mut next = old.clone();
            next.replace_range(start + offset..start + offset + c.len_utf8(), "");
            assert_eq!(
                atomic_edit(&old, &next, &mentions()),
                Some(("oi  fim".into(), start))
            );
        }
        let next = old.replacen("@Ana", "@Axna", 1);
        assert_eq!(
            atomic_edit(&old, &next, &mentions()),
            Some(("oi x fim".into(), 4))
        );
        for (a, b) in [
            (start + 1, end),
            (start, end - 1),
            (0, old.len()),
            (end, start),
            (usize::MAX, usize::MAX),
        ] {
            assert_eq!(remove_mention(&old, &mentions(), a, b), None);
        }
    }

    #[test]
    fn padded_navigation_skips_tokens_and_preserves_shift_anchor() {
        let token = padded("João");
        let text = format!("oi {token} fim");
        let spans = mention_spans(&text, &mentions());
        let (start, end) = (spans[0].start, spans[0].end);
        assert_eq!(
            mention_navigation(&spans, start, start, false, false),
            Some((end, end))
        );
        assert_eq!(
            mention_navigation(&spans, end, end, true, false),
            Some((start, start))
        );
        assert_eq!(
            mention_navigation(&spans, start, 0, false, true),
            Some((0, end))
        );
        assert_eq!(
            mention_navigation(&spans, end, text.len(), true, true),
            Some((text.len(), start))
        );
        assert_eq!(
            mention_navigation(&spans, end, start + 2, true, false),
            Some((start, start))
        );
        assert_eq!(
            mention_navigation(&spans, start, end - 2, false, false),
            Some((end, end))
        );
        assert_eq!(mention_navigation(&spans, end, end, false, false), None);
        assert_eq!(mention_navigation(&spans, start, start, true, false), None);
        assert_eq!(expand_selection(&spans, start + 2, end - 2), (start, end));
    }

    #[test]
    fn spans_prefer_longest_whole_tokens() {
        let text = "@Ana Maria @Ana! mail@Ana @Anabela x@Ana @Ana_ @Ana@other";
        let spans = mention_spans(text, &mentions());
        assert_eq!(spans.len(), 2);
        assert_eq!(&text[spans[0].start..spans[0].end], "@Ana Maria");
        assert_eq!(spans[0].mention, 1);
        assert_eq!(&text[spans[1].start..spans[1].end], "@Ana");
    }

    #[test]
    fn unicode_offsets_and_punctuation_are_safe() {
        let text = "ação (@João)，@Ana🙂";
        let spans = mention_spans(text, &mentions());
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].start, "ação (".len());
        assert_eq!(&text[spans[0].start..spans[0].end], "@João");
        for offset in 0..=text.len() + 2 {
            assert!(text.is_char_boundary(byte_offset(text, offset)));
        }
        assert!(mention_spans("@Ana\u{301}", &mentions()).is_empty());
    }

    #[test]
    fn partial_deletion_and_replacement_remove_entire_mentions() {
        let old = "oi @Ana Maria fim";
        assert_eq!(
            atomic_edit(old, "oi @Ana Mari fim", &mentions()),
            Some(("oi  fim".into(), 3))
        );
        assert_eq!(
            atomic_edit(old, "oi @Axria fim", &mentions()),
            Some(("oi x fim".into(), 4))
        );
        assert_eq!(
            atomic_edit("@João!", "@Jox!", &mentions()),
            Some(("x!".into(), 1))
        );
    }

    #[test]
    fn insertion_inside_mentions_is_atomic_but_adjacent_typing_is_not() {
        assert_eq!(
            atomic_edit("@Ana", "@Axna", &mentions()),
            Some(("x".into(), 1))
        );
        assert_eq!(atomic_edit("@Ana", "@Ana!", &mentions()), None);
        assert_eq!(atomic_edit("@Ana", "x@Ana", &mentions()), None);
        assert_eq!(atomic_edit("@Ana", "@Ana", &mentions()), None);
    }

    #[test]
    fn selection_expands_in_both_directions_across_tokens() {
        let spans = mention_spans("@Ana e @João", &mentions());
        assert_eq!(expand_selection(&spans, 2, 10), (0, 13));
        assert_eq!(expand_selection(&spans, 10, 2), (0, 13));
        assert_eq!(expand_selection(&spans, 4, 7), (4, 7));
    }
}
