// TypeScript-side translations (UI strings produced outside .slint, e.g.
// connection status, previews, notifications). Slint markup strings use
// @tr() with the gettext catalog under i18n/ instead.

export type Locale = "en" | "pt";

let locale: Locale = "en";

export function setLocale(l: Locale) {
  locale = l;
}

export function currentLocale(): Locale {
  return locale;
}

const en = {
  "status.connecting": "Connecting...",
  "status.connected": "Connected",
  "status.scanQr": "Scan the QR code with WhatsApp on your phone",
  "status.sessionEnded": "Session ended. Generating a new QR...",
  "status.reconnecting": "Connection lost. Reconnecting...",
  "status.connectFailed": "Failed to connect: {0}",
  "status.pairingHint": "Enter the code on your phone: Linked devices → Link with phone number",
  "status.loggingOut": "Disconnecting...",
  "pairing.generating": "Generating...",
  "pairing.failed": "Failed to generate code",
  "error.invalidNumber": "Invalid phone number",
  "error.stillConnecting": "Still connecting, try again",
  "error.notConnected": "Not connected",
  "pin.wrong": "Wrong PIN",
  "pin.wrongCurrent": "Current PIN is incorrect",
  "pin.format": "PIN must be 4 to 10 digits",
  "pin.saved": "PIN saved. Local data is now protected by it.",
  "pin.removed": "PIN removed. Local data stays encrypted via your Windows account.",
  "media.unavailable": "Media unavailable",
  "media.imageUnavailable": "Image unavailable",
  "preview.photo": "📷 Photo",
  "preview.audio": "🎵 Audio",
  "preview.document": "📄 {0}",
  "doc.fallbackName": "Document",
  "day.today": "Today",
  "day.yesterday": "Yesterday",
  "presence.typing": "typing...",
  "presence.recording": "recording audio...",
  "presence.online": "online",
  "sync.older": "Syncing older messages...",
  "notify.newMessages": "{0} new messages ({1})",
  "notify.fromOne": "{0} new messages from {1}",
  "notify.appName": "Zapive",
  "picker.images": "Images",
  "picker.audio": "Audio",
  "picker.all": "All files",
  "lang.restart": "Language saved. Restart Zapive to apply.",
  "msg.deleted": "This message was deleted",
  "msg.forwarded": "Forwarded",
  "tray.open": "Open Zapive",
  "tray.exit": "Quit",
  "preview.sticker": "🩵 Sticker",
  "preview.video": "🎬 Video",
  "rec.noMic": "No microphone found (install ffmpeg to record)",
  "info.members": "{0} participants",
  "gif.noResults": "No GIFs found",
  "fav.empty": "Star a sticker on your phone to keep it here",
  "call.incomingVoice": "Incoming voice call",
  "call.incomingVideo": "Incoming video call",
  "call.answered": "Answered elsewhere",
  "call.declined": "Declined",
  "call.missed": "Missed call",
  "call.ended": "Call ended",
  "call.ringing": "Ringing",
  "call.video": "Video call",
  "call.voice": "Voice call",
  "preview.gif": "🎬 GIF",
  "preview.reacted": 'Reacted {0} to: "{1}"',
  "forward.title": "Forward message to",
} as const;

export type MessageKey = keyof typeof en;

const pt: Record<MessageKey, string> = {
  "status.connecting": "Conectando...",
  "status.connected": "Conectado",
  "status.scanQr": "Escaneie o QR code com o WhatsApp do celular",
  "status.sessionEnded": "Sessão encerrada. Gerando novo QR...",
  "status.reconnecting": "Conexão perdida. Reconectando...",
  "status.connectFailed": "Falha ao conectar: {0}",
  "status.pairingHint": "Digite o código no celular: Aparelhos conectados → Conectar com número",
  "status.loggingOut": "Desconectando...",
  "pairing.generating": "Gerando...",
  "pairing.failed": "Falha ao gerar código",
  "error.invalidNumber": "Número de telefone inválido",
  "error.stillConnecting": "Ainda conectando, tente novamente",
  "error.notConnected": "Não conectado",
  "pin.wrong": "PIN incorreto",
  "pin.wrongCurrent": "PIN atual incorreto",
  "pin.format": "PIN deve ter de 4 a 10 dígitos",
  "pin.saved": "PIN salvo. Os dados locais agora são protegidos por ele.",
  "pin.removed": "PIN removido. Os dados locais seguem criptografados pela sua conta Windows.",
  "media.unavailable": "Mídia indisponível",
  "media.imageUnavailable": "Imagem indisponível",
  "preview.photo": "📷 Foto",
  "preview.audio": "🎵 Áudio",
  "preview.document": "📄 {0}",
  "doc.fallbackName": "Documento",
  "day.today": "Hoje",
  "day.yesterday": "Ontem",
  "presence.typing": "digitando...",
  "presence.recording": "gravando áudio...",
  "presence.online": "online",
  "sync.older": "Sincronizando mensagens antigas...",
  "notify.newMessages": "{0} novas mensagens ({1})",
  "notify.fromOne": "{0} novas mensagens de {1}",
  "notify.appName": "Zapive",
  "picker.images": "Imagens",
  "picker.audio": "Áudio",
  "picker.all": "Todos os arquivos",
  "lang.restart": "Idioma salvo. Reinicie o Zapive para aplicar.",
  "msg.deleted": "Essa mensagem foi apagada",
  "msg.forwarded": "Encaminhada",
  "tray.open": "Abrir Zapive",
  "tray.exit": "Sair",
  "preview.sticker": "🩵 Figurinha",
  "preview.video": "🎬 Vídeo",
  "rec.noMic": "Nenhum microfone encontrado (instale o ffmpeg para gravar)",
  "info.members": "{0} participantes",
  "gif.noResults": "Nenhum GIF encontrado",
  "fav.empty": "Marque uma figurinha com estrela no celular para guard\u00e1-la aqui",
  "call.incomingVoice": "Chamada de voz recebida",
  "call.incomingVideo": "Chamada de v\u00eddeo recebida",
  "call.answered": "Atendida em outro aparelho",
  "call.declined": "Recusada",
  "call.missed": "Chamada perdida",
  "call.ended": "Chamada encerrada",
  "call.ringing": "Chamando",
  "call.video": "Chamada de v\u00eddeo",
  "call.voice": "Chamada de voz",
  "preview.gif": "🎬 GIF",
  "preview.reacted": 'Reagiu {0} a: "{1}"',
  "forward.title": "Encaminhar mensagem para",
};

export function t(key: MessageKey, ...args: (string | number)[]): string {
  const table: Record<MessageKey, string> = locale === "pt" ? pt : en;
  let out: string = table[key] ?? en[key];
  args.forEach((a, i) => {
    out = out.replace(`{${i}}`, String(a));
  });
  return out;
}
