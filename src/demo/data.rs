// The invented world the demo shows: fictional people and groups, generated
// artwork, and a conversation that exercises every kind of bubble.
// Nothing here comes from a real account.
use slint::{ModelRc, SharedString, VecModel};

use super::art;
use crate::i18n::{t, ta};
use crate::{
    AccountItem, AppWindow, CallItem, ChatItem, MemberItem, MessageItem, StickerCell,
};

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn strings(items: &[String]) -> ModelRc<SharedString> {
    model(items.iter().map(|s| SharedString::from(s.as_str())).collect())
}

// Same spreading as the app's own avatar colours, over a fake id.
fn color_idx(id: &str) -> i32 {
    let sum = id.encode_utf16().fold(0i32, |h, u| h.wrapping_add(u as i32));
    sum.abs() % 8
}

fn initial(name: &str) -> SharedString {
    name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into()
}

struct Person {
    id: &'static str,
    name: &'static str,
    seed: u32,
    photo: bool,
}

const HELENA: Person = Person { id: "demo-helena", name: "Helena Park", seed: 11, photo: true };
const THEO: Person = Person { id: "demo-theo", name: "Theo Marques", seed: 23, photo: true };
const NINA: Person = Person { id: "demo-nina", name: "Nina Okafor", seed: 37, photo: true };
const OMAR: Person = Person { id: "demo-omar", name: "Omar Haddad", seed: 41, photo: false };
const IVY: Person = Person { id: "demo-ivy", name: "Ivy Lindqvist", seed: 53, photo: true };
const RAUL: Person = Person { id: "demo-raul", name: "Raul Tanaka", seed: 67, photo: false };
const SOFIA: Person = Person { id: "demo-sofia", name: "Sofia Bellini", seed: 71, photo: true };

fn face(person: &Person) -> (slint::Image, bool) {
    if person.photo {
        (art::avatar(person.seed), true)
    } else {
        (slint::Image::default(), false)
    }
}

struct Row<'a> {
    id: &'a str,
    name: String,
    seed: u32,
    photo: bool,
    preview: String,
    time: &'a str,
    unread: i32,
    pinned: bool,
    muted: bool,
    mentioned: bool,
    ticks: &'a str,
    blue: bool,
    draft: bool,
    typing: bool,
}

impl Row<'_> {
    fn item(self) -> ChatItem {
        ChatItem {
            jid: self.id.into(),
            name: self.name.as_str().into(),
            preview: self.preview.into(),
            typing: self.typing,
            draft: self.draft,
            ticks: self.ticks.into(),
            ticksBlue: self.blue,
            time: self.time.into(),
            avatar: if self.photo { art::avatar(self.seed) } else { slint::Image::default() },
            hasAvatar: self.photo,
            initial: initial(&self.name),
            colorIdx: color_idx(self.id),
            unread: self.unread,
            mentioned: self.mentioned,
            pinned: self.pinned,
            muted: self.muted,
        }
    }
}

fn row<'a>(id: &'a str, name: &str, seed: u32, photo: bool, preview: &str, time: &'a str) -> Row<'a> {
    Row {
        id,
        name: name.to_string(),
        seed,
        photo,
        preview: preview.to_string(),
        time,
        unread: 0,
        pinned: false,
        muted: false,
        mentioned: false,
        ticks: "",
        blue: false,
        draft: false,
        typing: false,
    }
}

fn chat_list(en: bool, open_jid: &str) -> Vec<ChatItem> {
    let l = |a: &str, b: &str| if en { a.to_string() } else { b.to_string() };
    let mut rows = vec![
        Row { pinned: true, ticks: "✓✓", blue: true, ..row(HELENA.id, HELENA.name, HELENA.seed, true, &l("Sounds perfect, see you there!", "Combinado, te vejo lá!"), "09:41") },
        Row { pinned: true, unread: 3, ..row("demo-group-hikers", "Sunday Hikers", 101, true, &l("Nina: Who is bringing the thermos?", "Nina: Quem leva a garrafa térmica?"), "09:12") },
        Row { pinned: true, muted: true, ..row("demo-group-family", &l("Family 🏡", "Família 🏡"), 103, true, &l("Mom: Dinner is at 8", "Mãe: O jantar é às 20h"), "08:30") },
        Row { unread: 2, mentioned: true, ..row("demo-group-design", "Design Crit", 107, true, &l("Theo: @you can you review the mocks?", "Theo: @você pode revisar os layouts?"), "Yesterday") },
        Row { typing: true, ..row(THEO.id, THEO.name, THEO.seed, true, &t("presence.typing"), "09:38") },
        Row { ticks: "✓✓", ..row(NINA.id, NINA.name, NINA.seed, true, &l("🎙 Voice message (0:23)", "🎙 Mensagem de voz (0:23)"), "Yesterday") },
        Row { unread: 1, ..row(OMAR.id, OMAR.name, OMAR.seed, false, &l("📷 Photo", "📷 Foto"), "Yesterday") },
        Row { draft: true, ..row(IVY.id, IVY.name, IVY.seed, true, &format!("{} {}", t("preview.draft"), l("Let me check and get back", "Vou ver e te respondo")), "Mon") },
        Row { ticks: "✓", ..row(RAUL.id, RAUL.name, RAUL.seed, false, &l("The invoice is attached 📄", "Segue a fatura em anexo 📄"), "Mon") },
        Row { muted: true, ..row("demo-group-book", "Book Club 📚", 109, true, &l("Sofia: Chapter 7 was wild", "Sofia: O capítulo 7 foi demais"), "Sun") },
        row(SOFIA.id, SOFIA.name, SOFIA.seed, true, &l("Thanks again for the recipe!", "Obrigada de novo pela receita!"), "Sat"),
        row("demo-group-trip", "Lisbon Trip ✈️", 113, true, &l("Ivy: Booked the apartment", "Ivy: Reservei o apartamento"), "Fri"),
    ];
    let _ = open_jid;
    rows.iter_mut().for_each(|r| r.preview = r.preview.replace('\n', " "));
    rows.into_iter().map(Row::item).collect()
}

fn base_message() -> MessageItem {
    MessageItem { mediaProgress: -1.0, ..Default::default() }
}

struct Msg {
    inner: MessageItem,
}

impl Msg {
    fn text(id: &str, text: &str, time: &str, from_me: bool) -> Msg {
        Msg {
            inner: MessageItem {
                id: id.into(),
                kind: "text".into(),
                text: text.into(),
                time: time.into(),
                fromMe: from_me,
                ..base_message()
            },
        }
    }

    fn ticks(mut self, ticks: &str, blue: bool) -> Msg {
        self.inner.ticks = ticks.into();
        self.inner.ticksBlue = blue;
        self
    }

    fn react(mut self, emoji: &str, count: &str) -> Msg {
        self.inner.reactions = emoji.into();
        self.inner.reactionCount = count.into();
        self
    }

    fn reply(mut self, name: &str, text: &str) -> Msg {
        self.inner.hasQuote = true;
        self.inner.quoteName = name.into();
        self.inner.quoteText = text.into();
        self.inner.quoteId = "demo-quote".into();
        self
    }

    fn from(mut self, person: &Person, show_name: bool) -> Msg {
        let (avatar, has) = face(person);
        self.inner.sender = person.name.into();
        self.inner.senderJid = person.id.into();
        self.inner.senderLabel = person.name.into();
        self.inner.senderAvatar = avatar;
        self.inner.senderHasAvatar = has;
        self.inner.senderInitial = initial(person.name);
        self.inner.senderColorIdx = color_idx(person.id);
        self.inner.showSender = show_name;
        self.inner.groupIndent = true;
        self.inner.showAvatar = show_name;
        self
    }

    fn image(mut self, seed: u32, w: i32, h: i32) -> Msg {
        self.inner.kind = "image".into();
        self.inner.picture = art::photo(seed, w as u32, h as u32);
        self.inner.picW = w;
        self.inner.picH = h;
        self.inner.mediaReady = true;
        self
    }

    fn voice(mut self, who: &Person, length: &str, seed: u32, progress: f32) -> Msg {
        let (avatar, has) = face(who);
        self.inner.kind = "audio".into();
        self.inner.voiceMessage = true;
        self.inner.text = length.into();
        self.inner.wave = art::waveform(seed);
        self.inner.hasWave = true;
        self.inner.progress = progress;
        self.inner.voiceAvatar = avatar;
        self.inner.voiceHasAvatar = has;
        self.inner.voiceInitial = initial(who.name);
        self.inner.voiceColorIdx = color_idx(who.id);
        self.inner.mediaReady = true;
        self
    }

    fn document(mut self, name: &str, kind: &str, size: &str) -> Msg {
        self.inner.kind = "doc".into();
        self.inner.text = name.into();
        self.inner.documentType = kind.into();
        self.inner.mediaSize = size.into();
        self.inner.mediaReady = true;
        self
    }

    fn day(mut self, label: &str) -> Msg {
        self.inner.dayLabel = label.into();
        self
    }

    fn first(mut self) -> Msg {
        self.inner.firstOfRun = true;
        self
    }

    fn done(mut self, day: &str) -> MessageItem {
        self.inner.dayFull = day.into();
        self.inner
    }
}

fn dm_messages(en: bool) -> Vec<MessageItem> {
    let l = |a: &str, b: &str| if en { a.to_string() } else { b.to_string() };
    let today = t("day.today");
    let mut v = vec![
        Msg::text("m1", &l("Morning! Are we still on for the trail this weekend?", "Bom dia! Ainda vamos na trilha neste fim de semana?"), "09:02", false).first().day(&today),
        Msg::text("m2", &l("Yes! The forecast looks great ☀️", "Vamos! A previsão está ótima ☀️"), "09:04", true).first().ticks("✓✓", true).react("👍", ""),
        Msg::text("m4", &l("Sunrise from the ridge. Worth the 5 am alarm.", "Nascer do sol na crista. Valeu acordar às 5h."), "09:06", true).image(4, 330, 190).ticks("✓✓", true).react("❤️🔥", "2"),
        Msg::text("m5", &l("Wow, that is unreal 😍", "Uau, isso é surreal 😍"), "09:08", false).first().react("😮", ""),
        Msg::text("m6", "", "09:10", false).voice(&HELENA, "0:23", 8, 0.42),
        Msg::text("m7", &l("Here is the route and the packing list", "Segue o roteiro e a lista de itens"), "09:14", true).first().document(&l("trail-route-and-packing.pdf", "roteiro-e-itens-da-trilha.pdf"), "PDF", "1.2 MB").ticks("✓✓", false),
        Msg::text("m8", &l("Perfect. Should I bring the tent too?", "Perfeito. Levo a barraca também?"), "09:30", false).first().reply(&t("reactions.you"), &l("Here is the route and the packing list", "Segue o roteiro e a lista de itens")),
        Msg::text("m9", &l("Just the small one. We will camp near the lake.", "Só a pequena. Vamos acampar perto do lago."), "09:39", true).first().ticks("✓✓", true),
        Msg::text("m10", &l("Sounds perfect, see you there!", "Combinado, te vejo lá!"), "09:41", false).first(),
    ];
    for m in &mut v {
        if m.inner.fromMe && m.inner.firstOfRun {
            m.inner.senderLabel = "".into();
        }
    }
    v.into_iter().map(|m| m.done(&today)).collect()
}

fn group_messages(en: bool) -> Vec<MessageItem> {
    let l = |a: &str, b: &str| if en { a.to_string() } else { b.to_string() };
    let today = t("day.today");
    let v = vec![
        Msg::text("g1", &l("Meeting point is the old bridge, 7:30 sharp.", "Ponto de encontro na ponte velha, 7h30 em ponto."), "08:50", false).from(&NINA, true).first().day(&today),
        Msg::text("g2", &l("I will bring maps and snacks 🥜", "Levo mapas e lanches 🥜"), "08:52", false).from(&THEO, true).first(),
        Msg::text("g3", &l("Count me in!", "Podem contar comigo!"), "08:55", false).from(&HELENA, true).first().react("🙌", ""),
        Msg::text("g4", "", "09:01", false).voice(&NINA, "0:23", 21, 0.0).from(&NINA, true).first(),
        Msg::text("g5", &l("Last month's summit view, for motivation", "A vista do cume do mês passado, pra motivar"), "09:05", false).from(&IVY, true).first().image(6, 330, 190).react("😍🔥👏", "5"),
        Msg::text("g6", &l("Who is bringing the thermos?", "Quem leva a garrafa térmica?"), "09:12", false).from(&NINA, true).first().reply(&l("Theo Marques", "Theo Marques"), &l("I will bring maps and snacks 🥜", "Levo mapas e lanches 🥜")),
    ];
    v.into_iter().map(|m| m.done(&today)).collect()
}

fn member(person: &Person, admin: bool) -> MemberItem {
    let (avatar, has) = face(person);
    MemberItem {
        jid: person.id.into(),
        name: person.name.into(),
        number: "".into(),
        avatar,
        hasAvatar: has,
        initial: initial(person.name),
        colorIdx: color_idx(person.id),
        checked: false,
        admin,
    }
}

fn media_grid() -> ModelRc<ModelRc<StickerCell>> {
    let mut rows = Vec::new();
    for r in 0..2u32 {
        let cells = (0..4u32)
            .map(|c| StickerCell {
                id: format!("demo-media-{r}-{c}").into(),
                pic: art::photo(30 + r * 4 + c, 160, 160),
                ready: true,
            })
            .collect::<Vec<_>>();
        rows.push(model(cells));
    }
    model(rows)
}

fn calls(en: bool) -> Vec<CallItem> {
    let l = |a: &str, b: &str| if en { a.to_string() } else { b.to_string() };
    [(&HELENA, false, "12:04"), (&THEO, true, "Yesterday"), (&NINA, false, "Mon")]
        .into_iter()
        .enumerate()
        .map(|(i, (who, video, time))| {
            let (avatar, has) = face(who);
            CallItem {
                id: format!("demo-call-{i}").into(),
                from: who.id.into(),
                name: who.name.into(),
                detail: l(if video { "Video call · 14 min" } else { "Voice call · 3 min" }, if video { "Chamada de vídeo · 14 min" } else { "Chamada de voz · 3 min" }).into(),
                time: time.into(),
                avatar,
                hasAvatar: has,
                initial: initial(who.name),
                colorIdx: color_idx(who.id),
                video,
                group: false,
            }
        })
        .collect()
}

pub fn populate(ui: &AppWindow, en: bool, group: bool) {
    let l = |a: &str, b: &str| if en { a.to_string() } else { b.to_string() };

    ui.set_screen("main".into());
    ui.set_app_version(crate::update::current_version().into());
    ui.set_status_text(t("status.connected").into());
    ui.set_notifications(true);
    ui.set_notify_previews(true);
    ui.set_message_sound(true);

    let open_jid = if group { "demo-group-hikers" } else { HELENA.id };
    ui.set_chats(model(chat_list(en, open_jid)));
    ui.set_calls(model(calls(en)));
    ui.set_chats_unread_count(4);
    ui.set_archived_unread_count(3);
    ui.set_calls_unseen_count(0);
    ui.set_selected_jid(open_jid.into());
    ui.set_current_jid(open_jid.into());
    ui.set_chat_open(true);
    ui.set_conv_ready(true);
    ui.set_stick_bottom(true);
    ui.set_current_can_send(true);

    let me = AccountItem {
        id: "demo".into(),
        name: l("Demo Account", "Conta de demonstração").into(),
        detail: "".into(),
        initial: "D".into(),
        colorIdx: 1,
        avatar: art::avatar(5),
        hasAvatar: true,
        active: true,
    };
    ui.set_my_account(me.clone());
    ui.set_accounts(model(vec![me]));
    ui.set_account_count(1);

    let system = t("device.system");
    ui.set_mic_devices(strings(&[system.clone(), "Studio Microphone".into()]));
    ui.set_speaker_devices(strings(&[system.clone(), "Desk Speakers".into()]));
    ui.set_camera_devices(strings(&[system.clone(), "HD Webcam".into()]));
    ui.set_mic_device(system.clone().into());
    ui.set_speaker_device(system.clone().into());
    ui.set_camera_device(system.into());

    if group {
        let (avatar, _) = (art::avatar(101), true);
        ui.set_current_chat_name("Sunday Hikers".into());
        ui.set_current_avatar(avatar.clone());
        ui.set_current_avatar_has(true);
        ui.set_current_initial("S".into());
        ui.set_current_color_idx(color_idx(open_jid));
        ui.set_current_status(l("Helena, Theo, Nina, Ivy, You", "Helena, Theo, Nina, Ivy, Você").into());
        ui.set_current_is_group(true);
        ui.set_current_callable(false);
        ui.set_messages(model(group_messages(en)));

        ui.set_info_name("Sunday Hikers".into());
        ui.set_info_id("".into());
        ui.set_info_desc(l("Every Sunday, rain or shine. Bring water, good boots and a story to tell.", "Todo domingo, faça chuva ou sol. Leve água, bota boa e uma história pra contar.").into());
        ui.set_info_is_group(true);
        ui.set_info_members(ta("info.members", &["5"]).into());
        ui.set_info_avatar(avatar);
        ui.set_info_has_avatar(true);
        ui.set_info_initial("S".into());
        ui.set_info_color_idx(color_idx(open_jid));
        ui.set_info_media(media_grid());
        ui.set_info_member_rows(model(vec![
            member(&NINA, true),
            member(&THEO, true),
            member(&IVY, false),
            member(&HELENA, false),
            MemberItem {
                name: t("reactions.you").into(),
                ..member(&OMAR, false)
            },
        ]));
    } else {
        let (avatar, _) = face(&HELENA);
        ui.set_current_chat_name(HELENA.name.into());
        ui.set_current_avatar(avatar.clone());
        ui.set_current_avatar_has(true);
        ui.set_current_initial(initial(HELENA.name));
        ui.set_current_color_idx(color_idx(HELENA.id));
        ui.set_current_status(t("presence.online").into());
        ui.set_current_is_group(false);
        ui.set_current_callable(true);
        ui.set_messages(model(dm_messages(en)));

        ui.set_info_name(HELENA.name.into());
        ui.set_info_id("+00 000 000 000".into());
        ui.set_info_about(l("Out on a trail somewhere 🌲", "Em alguma trilha por aí 🌲").into());
        ui.set_info_avatar(avatar);
        ui.set_info_has_avatar(true);
        ui.set_info_initial(initial(HELENA.name));
        ui.set_info_color_idx(color_idx(HELENA.id));
        ui.set_info_media(media_grid());
    }
}
