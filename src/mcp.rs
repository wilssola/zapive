// In-process MCP server: lets local AI clients read and drive the running
// app over streamable HTTP on 127.0.0.1. Opt-in from Settings; every
// request needs the bearer token, and browser origins other than
// localhost are refused (DNS rebinding). Tool calls run on the UI thread
// through bridge::ui_apply and answer over a oneshot.
use async_trait::async_trait;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::mcp_http::{McpAppState, McpHttpHandler};
use rust_mcp_sdk::mcp_server::{ServerHandler, ToMcpServerHandler};
use rust_mcp_sdk::schema::schema_utils::CallToolError;
use rust_mcp_sdk::schema::{
    CallToolRequestParams, CallToolResult, Implementation, ListToolsResult,
    ListToolsResultCacheScope, PaginatedRequestParams, RpcError, ServerCapabilities,
    ServerCapabilitiesTools, ServerResult, TextContent,
};
use rust_mcp_sdk::{McpServer, RequestContext, ServerDetails, tool_box};
use serde_json::Value;
use std::sync::{Arc, RwLock};
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 47823;
pub const MAX_LIMIT: usize = 200;
const MAX_ID_LEN: usize = 128;
const MAX_QUERY_LEN: usize = 256;
const MAX_TEXT_LEN: usize = 65_536;
const MAX_EMOJI_LEN: usize = 32;
const CALL_TIMEOUT: Duration = Duration::from_secs(20);

// ---- tools ----

#[mcp_tool(
    name = "list_chats",
    description = "Lists WhatsApp chats, most recent first. Returns chat_id values used by the other tools.",
    read_only_hint = true,
    destructive_hint = false,
    idempotent_hint = true,
    open_world_hint = false
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct ListChats {
    /// Only chats whose name or id contains this text.
    pub query: Option<String>,
    /// Maximum number of chats (1-200, default 50).
    pub limit: Option<u32>,
}

#[mcp_tool(
    name = "read_messages",
    description = "Reads the latest messages of a chat in chronological order. Pass `before` (a message_id) to page further back.",
    read_only_hint = true,
    destructive_hint = false,
    idempotent_hint = true,
    open_world_hint = false
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct ReadMessages {
    /// Chat id from list_chats.
    pub chat_id: String,
    /// Maximum number of messages (1-200, default 50).
    pub limit: Option<u32>,
    /// Only messages older than this message_id.
    pub before: Option<String>,
}

#[mcp_tool(
    name = "search_messages",
    description = "Full-text search over stored messages, optionally within one chat.",
    read_only_hint = true,
    destructive_hint = false,
    idempotent_hint = true,
    open_world_hint = false
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct SearchMessages {
    /// Words to search for.
    pub query: String,
    /// Restrict the search to this chat.
    pub chat_id: Option<String>,
    /// Maximum number of hits (1-200, default 50).
    pub limit: Option<u32>,
}

#[mcp_tool(
    name = "get_chat_info",
    description = "Details about one chat: name, type, unread count, mute/archive/pin state and group members.",
    read_only_hint = true,
    destructive_hint = false,
    idempotent_hint = true,
    open_world_hint = false
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct GetChatInfo {
    /// Chat id from list_chats.
    pub chat_id: String,
}

#[mcp_tool(
    name = "send_message",
    description = "Sends a text message to an existing chat as the logged-in user. Optionally replies to a message.",
    read_only_hint = false,
    destructive_hint = false,
    idempotent_hint = false,
    open_world_hint = true
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct SendMessage {
    /// Chat id from list_chats.
    pub chat_id: String,
    /// Message text (WhatsApp formatting allowed).
    pub text: String,
    /// message_id to reply to.
    pub reply_to: Option<String>,
}

#[mcp_tool(
    name = "mark_as_read",
    description = "Marks a chat as read.",
    read_only_hint = false,
    destructive_hint = false,
    idempotent_hint = true,
    open_world_hint = true
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct MarkAsRead {
    /// Chat id from list_chats.
    pub chat_id: String,
}

#[mcp_tool(
    name = "react_to_message",
    description = "Reacts to a message with an emoji. An empty emoji removes your reaction.",
    read_only_hint = false,
    destructive_hint = false,
    idempotent_hint = true,
    open_world_hint = true
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct ReactToMessage {
    /// Chat id from list_chats.
    pub chat_id: String,
    /// message_id from read_messages or search_messages.
    pub message_id: String,
    /// A single emoji, or empty to remove the reaction.
    pub emoji: String,
}

tool_box!(
    ZapiveTools,
    [ListChats, ReadMessages, SearchMessages, GetChatInfo, SendMessage, MarkAsRead, ReactToMessage]
);

// A tool call with its arguments checked and limits capped.
#[derive(Debug, PartialEq)]
pub enum Call {
    ListChats { query: Option<String>, limit: usize },
    ReadMessages { chat_id: String, limit: usize, before: Option<String> },
    SearchMessages { query: String, chat_id: Option<String>, limit: usize },
    GetChatInfo { chat_id: String },
    SendMessage { chat_id: String, text: String, reply_to: Option<String> },
    MarkAsRead { chat_id: String },
    React { chat_id: String, message_id: String, emoji: String },
}

impl TryFrom<ZapiveTools> for Call {
    type Error = String;

    fn try_from(tool: ZapiveTools) -> Result<Self, String> {
        Ok(match tool {
            ZapiveTools::ListChats(t) => Call::ListChats {
                query: optional(t.query, |q| query(&q))?,
                limit: limit(t.limit),
            },
            ZapiveTools::ReadMessages(t) => Call::ReadMessages {
                chat_id: chat_id(&t.chat_id)?,
                limit: limit(t.limit),
                before: optional(t.before, |id| message_id(&id))?,
            },
            ZapiveTools::SearchMessages(t) => Call::SearchMessages {
                query: query(&t.query)?,
                chat_id: optional(t.chat_id, |id| chat_id(&id))?,
                limit: limit(t.limit),
            },
            ZapiveTools::GetChatInfo(t) => Call::GetChatInfo { chat_id: chat_id(&t.chat_id)? },
            ZapiveTools::SendMessage(t) => Call::SendMessage {
                chat_id: chat_id(&t.chat_id)?,
                text: text(&t.text)?,
                reply_to: optional(t.reply_to, |id| message_id(&id))?,
            },
            ZapiveTools::MarkAsRead(t) => Call::MarkAsRead { chat_id: chat_id(&t.chat_id)? },
            ZapiveTools::ReactToMessage(t) => Call::React {
                chat_id: chat_id(&t.chat_id)?,
                message_id: message_id(&t.message_id)?,
                emoji: emoji(&t.emoji)?,
            },
        })
    }
}

pub fn limit(value: Option<u32>) -> usize {
    value.map_or(50, |v| v as usize).clamp(1, MAX_LIMIT)
}

// Absent and blank optional arguments both mean "not given".
fn optional(
    value: Option<String>,
    check: impl FnOnce(String) -> Result<String, String>,
) -> Result<Option<String>, String> {
    match value {
        Some(v) if !v.trim().is_empty() => check(v).map(Some),
        _ => Ok(None),
    }
}

fn token_like(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_ID_LEN && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub fn chat_id(value: &str) -> Result<String, String> {
    let value = value.trim();
    if token_like(value) && value.contains('@') {
        Ok(value.to_string())
    } else {
        Err("Invalid chat_id: expected a chat id from list_chats, such as 5511999999999@s.whatsapp.net or 1203...@g.us.".into())
    }
}

pub fn message_id(value: &str) -> Result<String, String> {
    let value = value.trim();
    if token_like(value) {
        Ok(value.to_string())
    } else {
        Err("Invalid message_id: expected a message_id from read_messages or search_messages.".into())
    }
}

fn query(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > MAX_QUERY_LEN {
        Err(format!("Invalid query: expected 1-{MAX_QUERY_LEN} characters."))
    } else {
        Ok(value.to_string())
    }
}

fn text(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > MAX_TEXT_LEN {
        Err(format!("Invalid text: expected 1-{MAX_TEXT_LEN} characters."))
    } else {
        Ok(value.to_string())
    }
}

fn emoji(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.chars().count() > MAX_EMOJI_LEN || value.chars().any(|c| c.is_ascii_alphanumeric() || c.is_control()) {
        Err("Invalid emoji: expected a single emoji, or empty to remove the reaction.".into())
    } else {
        Ok(value.to_string())
    }
}

struct Handler;

#[async_trait]
impl ServerHandler for Handler {
    async fn handle_list_tools_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: &RequestContext,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            cache_scope: ListToolsResultCacheScope::Private,
            result_type: "complete".to_string(),
            ttl_ms: 0,
            meta: None,
            next_cursor: None,
            tools: ZapiveTools::tools(),
        })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _context: &RequestContext,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ServerResult, CallToolError> {
        let tool = ZapiveTools::try_from(params)?;
        let call = Call::try_from(tool).map_err(CallToolError::from_message)?;
        let value = dispatch(call).await.map_err(CallToolError::from_message)?;
        let text = serde_json::to_string_pretty(&value).unwrap_or_default();
        Ok(CallToolResult::text_content(vec![TextContent::from(text)]).into())
    }
}

async fn dispatch(call: Call) -> Result<Value, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    crate::bridge::ui_apply(move |b| {
        let _ = tx.send(b.mcp_call(call));
    });
    match tokio::time::timeout(CALL_TIMEOUT, rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("Zapive is not ready (starting up or shutting down).".into()),
        Err(_) => Err("Zapive did not answer in time.".into()),
    }
}

// ---- HTTP ----

pub fn new_token() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn bearer_matches(header: Option<&str>, token: &str) -> bool {
    let Some(value) = header else { return false };
    let Some((scheme, given)) = value.trim().split_once(' ') else { return false };
    !token.is_empty() && scheme.eq_ignore_ascii_case("bearer") && constant_time_eq(given.trim().as_bytes(), token.as_bytes())
}

fn loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

// No Origin (a desktop client) is fine; a browser page must be local.
pub fn origin_allowed(origin: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    let Ok(url) = url::Url::parse(origin) else { return false };
    matches!(url.scheme(), "http" | "https") && url.host_str().is_some_and(loopback_host)
}

// The Host a rebinding page would send is its own domain, not ours.
pub fn host_allowed(host: Option<&str>, port: u16) -> bool {
    let Some(host) = host else { return false };
    let Ok(url) = url::Url::parse(&format!("http://{host}")) else { return false };
    url.host_str().is_some_and(loopback_host) && url.port_or_known_default() == Some(port)
}

pub fn client_config(url: &str, token: &str) -> String {
    serde_json::json!({
        "mcpServers": {
            "zapive": {
                "type": "http",
                "url": url,
                "headers": { "Authorization": format!("Bearer {token}") }
            }
        }
    })
    .to_string()
}

pub fn url_for(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

struct Shared {
    port: u16,
    // None once the server is stopped: stragglers get 503.
    token: RwLock<Option<String>>,
}

impl Shared {
    fn reject(&self, headers: &HeaderMap) -> Option<StatusCode> {
        let text = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
        if !origin_allowed(text(header::ORIGIN)) || !host_allowed(text(header::HOST), self.port) {
            return Some(StatusCode::FORBIDDEN);
        }
        let token = self.token.read().ok()?.clone();
        let Some(token) = token else { return Some(StatusCode::SERVICE_UNAVAILABLE) };
        if !bearer_matches(text(header::AUTHORIZATION), &token) {
            return Some(StatusCode::UNAUTHORIZED);
        }
        None
    }
}

struct App {
    shared: Arc<Shared>,
    state: Arc<McpAppState>,
    http: McpHttpHandler,
}

async fn serve_mcp(
    State(app): State<Arc<App>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(status) = app.shared.reject(&headers) {
        let mut response = status.into_response();
        if status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(header::WWW_AUTHENTICATE, header::HeaderValue::from_static("Bearer"));
        }
        return response;
    }
    let Ok(payload) = std::str::from_utf8(&body) else {
        return (StatusCode::BAD_REQUEST, "body must be UTF-8").into_response();
    };
    let request = McpHttpHandler::create_request(method, uri, headers, Some(payload));
    match app.http.handle_streamable_http(request, app.state.clone()).await {
        Ok(response) => {
            let (parts, body) = response.into_parts();
            Response::from_parts(parts, Body::new(body))
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn app_state() -> Arc<McpAppState> {
    let details = ServerDetails {
        server_info: Implementation {
            name: "zapive".into(),
            title: Some("Zapive".into()),
            version: env!("CARGO_PKG_VERSION").into(),
            description: Some("WhatsApp through the running Zapive desktop app".into()),
            icons: Vec::new(),
            website_url: None,
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools { list_changed: None }),
            ..Default::default()
        },
        instructions: Some(
            "Reads and sends WhatsApp messages as the user logged into Zapive. Use list_chats to find a chat_id first. Confirm with the user before sending messages or reactions.".into(),
        ),
        meta: None,
    };
    Arc::new(McpAppState {
        id_generator: Arc::new(rust_mcp_sdk::id_generator::UuidGenerator {}),
        stream_id_gen: Arc::new(rust_mcp_sdk::id_generator::FastIdGenerator::new(Some("s_"))),
        server_details: Arc::new(details),
        handler: Handler.to_mcp_server_handler(),
        ping_interval: Duration::from_secs(12),
        transport_options: Arc::new(rust_mcp_sdk::TransportOptions::default()),
        enable_json_response: true,
        message_observer: None,
        extensions: Arc::new(tokio::sync::RwLock::new(None)),
        active_listen_streams: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        max_listen_streams: 16,
    })
}

// A running server; dropping it stops it.
pub struct Server {
    shared: Arc<Shared>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Server {
    pub fn url(&self) -> String {
        url_for(self.shared.port)
    }

    pub fn set_token(&self, token: &str) {
        if let Ok(mut slot) = self.shared.token.write() {
            *slot = Some(token.to_string());
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.shared.token.write() {
            *slot = None;
        }
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

// Binds 127.0.0.1:`port`, or any free port when that one is taken.
pub fn start(rt: &tokio::runtime::Handle, port: u16, token: &str) -> Result<Server, String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))
        .or_else(|_| std::net::TcpListener::bind(("127.0.0.1", 0)))
        .map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let shared = Arc::new(Shared { port, token: RwLock::new(Some(token.to_string())) });
    let app = Arc::new(App {
        shared: shared.clone(),
        state: app_state(),
        http: McpHttpHandler::new(Vec::new(), None),
    });
    let router = axum::Router::new()
        .route("/mcp", axum::routing::any(serve_mcp))
        .with_state(app);
    let listener = {
        let _guard = rt.enter();
        tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?
    };
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    rt.spawn(async move {
        let shutdown = async move {
            let _ = rx.await;
        };
        if let Err(e) = axum::serve(listener, router).with_graceful_shutdown(shutdown).await {
            eprintln!("[mcp] server stopped: {e}");
        }
    });
    Ok(Server { shared, shutdown: Some(tx) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn token_is_64_hex_chars_and_random() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn bearer_compare() {
        let token = "ab".repeat(32);
        assert!(bearer_matches(Some(&format!("Bearer {token}")), &token));
        assert!(bearer_matches(Some(&format!("bearer  {token} ")), &token));
        assert!(!bearer_matches(Some(&format!("Bearer {token}x")), &token));
        assert!(!bearer_matches(Some(&format!("Basic {token}")), &token));
        assert!(!bearer_matches(Some(&token), &token));
        assert!(!bearer_matches(None, &token));
        assert!(!bearer_matches(Some("Bearer "), ""));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn origin_and_host_checks() {
        assert!(origin_allowed(None));
        for ok in ["http://localhost", "http://localhost:3000", "https://127.0.0.1:8443", "http://[::1]:1"] {
            assert!(origin_allowed(Some(ok)), "{ok}");
        }
        for bad in ["null", "http://evil.com", "http://localhost.evil.com", "http://127.0.0.1.nip.io", "file:///x", "chrome-extension://abc"] {
            assert!(!origin_allowed(Some(bad)), "{bad}");
        }
        assert!(host_allowed(Some("127.0.0.1:47823"), 47823));
        assert!(host_allowed(Some("localhost:47823"), 47823));
        assert!(!host_allowed(Some("127.0.0.1:1"), 47823));
        assert!(!host_allowed(Some("evil.com:47823"), 47823));
        assert!(!host_allowed(None, 47823));
    }

    #[test]
    fn limits_are_capped() {
        assert_eq!(limit(None), 50);
        assert_eq!(limit(Some(0)), 1);
        assert_eq!(limit(Some(10)), 10);
        assert_eq!(limit(Some(5000)), MAX_LIMIT);
    }

    #[test]
    fn arguments_are_validated() {
        assert_eq!(chat_id(" 5511@s.whatsapp.net ").as_deref(), Ok("5511@s.whatsapp.net"));
        assert!(chat_id("").is_err());
        assert!(chat_id("5511999").is_err());
        assert!(chat_id("a b@g.us").is_err());
        assert!(chat_id(&format!("{}@g.us", "1".repeat(200))).is_err());
        assert!(message_id("3EB0ABC").is_ok());
        assert!(message_id("").is_err());
        assert!(emoji("👍").is_ok());
        assert!(emoji("").is_ok());
        assert!(emoji("hello").is_err());
        let call = Call::try_from(ZapiveTools::SendMessage(SendMessage {
            chat_id: "1@s.whatsapp.net".into(),
            text: "   ".into(),
            reply_to: None,
        }));
        assert!(call.is_err());
        let call = Call::try_from(ZapiveTools::ReadMessages(ReadMessages {
            chat_id: "1@g.us".into(),
            limit: Some(999),
            before: Some(" ".into()),
        }));
        assert_eq!(call, Ok(Call::ReadMessages { chat_id: "1@g.us".into(), limit: MAX_LIMIT, before: None }));
        let call = Call::try_from(ZapiveTools::SearchMessages(SearchMessages {
            query: "x".repeat(MAX_QUERY_LEN + 1),
            chat_id: None,
            limit: None,
        }));
        assert!(call.is_err());
    }

    #[test]
    fn config_json() {
        let config: Value = serde_json::from_str(&client_config(&url_for(47823), "tok")).unwrap();
        let server = &config["mcpServers"]["zapive"];
        assert_eq!(server["type"], "http");
        assert_eq!(server["url"], "http://127.0.0.1:47823/mcp");
        assert_eq!(server["headers"]["Authorization"], "Bearer tok");
    }

    #[test]
    fn tools_have_schemas() {
        let tools = ZapiveTools::tools();
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            ["list_chats", "read_messages", "search_messages", "get_chat_info", "send_message", "mark_as_read", "react_to_message"]
        );
        let schema = serde_json::to_value(&tools[1].input_schema).unwrap();
        assert_eq!(schema["required"], serde_json::json!(["chat_id"]));
    }

    async fn post(port: u16, body: &str, method: &str, extra: &str) -> (u16, String) {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMcp-Method: {method}\r\nMCP-Protocol-Version: 2026-07-28\r\nConnection: close\r\n{extra}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).await.unwrap();
        let status = out.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        (status, out)
    }

    fn rpc(method: &str) -> String {
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": method,
            "params": { "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
                "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "1" }
            } }
        })
        .to_string()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_enforces_auth_and_answers() {
        let token = new_token();
        let server = start(&tokio::runtime::Handle::current(), 0, &token).unwrap();
        let port = server.shared.port;
        assert_eq!(server.url(), format!("http://127.0.0.1:{port}/mcp"));
        let auth = format!("Authorization: Bearer {token}\r\n");

        let (status, _) = post(port, &rpc("server/discover"), "server/discover", "").await;
        assert_eq!(status, 401);
        let (status, _) = post(port, &rpc("server/discover"), "server/discover", "Authorization: Bearer nope\r\n").await;
        assert_eq!(status, 401);
        let evil = format!("{auth}Origin: http://evil.com\r\n");
        let (status, _) = post(port, &rpc("server/discover"), "server/discover", &evil).await;
        assert_eq!(status, 403);

        let (status, body) = post(port, &rpc("server/discover"), "server/discover", &auth).await;
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("2026-07-28"), "{body}");
        let local = format!("{auth}Origin: http://localhost:5173\r\n");
        let (status, body) = post(port, &rpc("tools/list"), "tools/list", &local).await;
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("send_message") && body.contains("list_chats"), "{body}");

        let fresh = new_token();
        server.set_token(&fresh);
        let (status, _) = post(port, &rpc("tools/list"), "tools/list", &auth).await;
        assert_eq!(status, 401);
        let (status, _) = post(port, &rpc("tools/list"), "tools/list", &format!("Authorization: Bearer {fresh}\r\n")).await;
        assert_eq!(status, 200);

        drop(server);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err());
    }
}
