// Rust-side translations (UI strings produced outside .slint, e.g.
// connection status, previews, notifications). Slint markup strings use
// @tr() with the bundled catalog compiled from i18n/ instead.
use std::sync::atomic::{AtomicBool, Ordering};

static PT: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    Pt,
}

pub fn set_locale(locale: Locale) {
    PT.store(locale == Locale::Pt, Ordering::Relaxed);
}

pub fn current_locale() -> Locale {
    if PT.load(Ordering::Relaxed) { Locale::Pt } else { Locale::En }
}

fn en(key: &str) -> &'static str {
    match key {
        "status.connecting" => "Connecting...",
        "status.connected" => "Connected",
        "status.scanQr" => "Scan the QR code with WhatsApp on your phone",
        "status.sessionEnded" => "Session ended. Generating a new QR...",
        "status.reconnecting" => "Connection lost. Reconnecting...",
        "status.connectFailed" => "Failed to connect: {0}",
        "status.pairingHint" => "Enter the code on your phone: Linked devices → Link with phone number",
        "status.loggingOut" => "Disconnecting...",
        "pairing.generating" => "Generating...",
        "pairing.failed" => "Failed to generate code",
        "error.invalidNumber" => "Invalid phone number",
        "error.stillConnecting" => "Still connecting, try again",
        "error.notConnected" => "Not connected",
        "pin.wrong" => "Wrong PIN",
        "pin.wrongCurrent" => "Current PIN is incorrect",
        "pin.format" => "PIN must be 4 to 10 digits",
        "pin.saved" => "PIN saved. Local data is now protected by it.",
        "pin.removed" => "PIN removed. Local data stays encrypted via your Windows account.",
        "media.unavailable" => "Media unavailable",
        "media.imageUnavailable" => "Image unavailable",
        "preview.photo" => "📷 Photo",
        "preview.audio" => "🎵 Audio",
        "preview.document" => "📄 {0}",
        "doc.fallbackName" => "Document",
        "day.today" => "Today",
        "day.yesterday" => "Yesterday",
        "presence.typing" => "typing...",
        "presence.recording" => "recording audio...",
        "presence.online" => "online",
        "sync.older" => "Syncing older messages...",
        "notify.newMessages" => "{0} new messages ({1})",
        "notify.fromOne" => "{0} new messages from {1}",
        "notify.appName" => "Zapive",
        "picker.images" => "Images",
        "picker.audio" => "Audio",
        "picker.all" => "All files",
        "lang.restart" => "Language saved. Restart Zapive to apply.",
        "msg.deleted" => "This message was deleted",
        "msg.forwarded" => "Forwarded",
        "tray.open" => "Open Zapive",
        "tray.exit" => "Quit",
        "preview.sticker" => "🩵 Sticker",
        "preview.video" => "🎬 Video",
        "rec.noMic" => "No microphone found",
        "info.members" => "{0} participants",
        "gif.noResults" => "No GIFs found",
        "fav.empty" => "Star a sticker on your phone to keep it here",
        "call.incomingVoice" => "Incoming voice call",
        "call.incomingVideo" => "Incoming video call",
        "call.answered" => "Answered elsewhere",
        "call.declined" => "Declined",
        "call.missed" => "Missed call",
        "call.ended" => "Call ended",
        "call.ringing" => "Ringing",
        "call.video" => "Video call",
        "call.voice" => "Voice call",
        "preview.gif" => "🎬 GIF",
        "preview.reacted" => "Reacted {0} to: \"{1}\"",
        "alert.conflictTitle" => "Session taken over",
        "alert.conflictBody" => "WhatsApp was opened somewhere else with this account. Only one session stays connected at a time.",
        "alert.reconnect" => "Use here",
        "alert.offlineTitle" => "Cannot reach WhatsApp",
        "alert.offlineBody" => "Zapive has been retrying without success. Check the connection and try again.",
        "alert.retry" => "Try again",
        "alert.close" => "Close",
        "reactions.count" => "{0} reactions",
        "reactions.you" => "You",
        "forward.title" => "Forward message to",
        _ => key_missing(key),
    }
}

fn pt(key: &str) -> &'static str {
    match key {
        "status.connecting" => "Conectando...",
        "status.connected" => "Conectado",
        "status.scanQr" => "Escaneie o QR code com o WhatsApp do celular",
        "status.sessionEnded" => "Sessão encerrada. Gerando novo QR...",
        "status.reconnecting" => "Conexão perdida. Reconectando...",
        "status.connectFailed" => "Falha ao conectar: {0}",
        "status.pairingHint" => "Digite o código no celular: Aparelhos conectados → Conectar com número",
        "status.loggingOut" => "Desconectando...",
        "pairing.generating" => "Gerando...",
        "pairing.failed" => "Falha ao gerar código",
        "error.invalidNumber" => "Número de telefone inválido",
        "error.stillConnecting" => "Ainda conectando, tente novamente",
        "error.notConnected" => "Não conectado",
        "pin.wrong" => "PIN incorreto",
        "pin.wrongCurrent" => "PIN atual incorreto",
        "pin.format" => "PIN deve ter de 4 a 10 dígitos",
        "pin.saved" => "PIN salvo. Os dados locais agora são protegidos por ele.",
        "pin.removed" => "PIN removido. Os dados locais seguem criptografados pela sua conta Windows.",
        "media.unavailable" => "Mídia indisponível",
        "media.imageUnavailable" => "Imagem indisponível",
        "preview.photo" => "📷 Foto",
        "preview.audio" => "🎵 Áudio",
        "preview.document" => "📄 {0}",
        "doc.fallbackName" => "Documento",
        "day.today" => "Hoje",
        "day.yesterday" => "Ontem",
        "presence.typing" => "digitando...",
        "presence.recording" => "gravando áudio...",
        "presence.online" => "online",
        "sync.older" => "Sincronizando mensagens antigas...",
        "notify.newMessages" => "{0} novas mensagens ({1})",
        "notify.fromOne" => "{0} novas mensagens de {1}",
        "notify.appName" => "Zapive",
        "picker.images" => "Imagens",
        "picker.audio" => "Áudio",
        "picker.all" => "Todos os arquivos",
        "lang.restart" => "Idioma salvo. Reinicie o Zapive para aplicar.",
        "msg.deleted" => "Essa mensagem foi apagada",
        "msg.forwarded" => "Encaminhada",
        "tray.open" => "Abrir Zapive",
        "tray.exit" => "Sair",
        "preview.sticker" => "🩵 Figurinha",
        "preview.video" => "🎬 Vídeo",
        "rec.noMic" => "Nenhum microfone encontrado",
        "info.members" => "{0} participantes",
        "gif.noResults" => "Nenhum GIF encontrado",
        "fav.empty" => "Marque uma figurinha com estrela no celular para guardá-la aqui",
        "call.incomingVoice" => "Chamada de voz recebida",
        "call.incomingVideo" => "Chamada de vídeo recebida",
        "call.answered" => "Atendida em outro aparelho",
        "call.declined" => "Recusada",
        "call.missed" => "Chamada perdida",
        "call.ended" => "Chamada encerrada",
        "call.ringing" => "Chamando",
        "call.video" => "Chamada de vídeo",
        "call.voice" => "Chamada de voz",
        "preview.gif" => "🎬 GIF",
        "preview.reacted" => "Reagiu {0} a: \"{1}\"",
        "alert.conflictTitle" => "Sessão assumida em outro lugar",
        "alert.conflictBody" => "O WhatsApp foi aberto em outro lugar com esta conta. Apenas uma sessão fica conectada por vez.",
        "alert.reconnect" => "Usar aqui",
        "alert.offlineTitle" => "Não foi possível conectar",
        "alert.offlineBody" => "O Zapive tentou reconectar várias vezes sem sucesso. Verifique a conexão e tente de novo.",
        "alert.retry" => "Tentar de novo",
        "alert.close" => "Fechar",
        "reactions.count" => "{0} reações",
        "reactions.you" => "Você",
        "forward.title" => "Encaminhar mensagem para",
        _ => key_missing(key),
    }
}

fn key_missing(key: &str) -> &'static str {
    debug_assert!(false, "missing i18n key: {key}");
    ""
}

pub fn t(key: &str) -> String {
    ta(key, &[])
}

// Positional {0}/{1} substitution, mirroring the Node build's t().
pub fn ta(key: &str, args: &[&str]) -> String {
    let raw = match current_locale() {
        Locale::Pt => {
            let s = pt(key);
            if s.is_empty() { en(key) } else { s }
        }
        Locale::En => en(key),
    };
    let mut out = if raw.is_empty() { key.to_string() } else { raw.to_string() };
    for (i, arg) in args.iter().enumerate() {
        out = out.replacen(&format!("{{{i}}}"), arg, 1);
    }
    out
}
