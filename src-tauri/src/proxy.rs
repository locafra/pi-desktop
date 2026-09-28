//! Proxy locale verso il server ttyd.
//!
//! Le webview (WebView2, WKWebView) non mostrano il login Basic Auth e le
//! WebSocket del browser non possono mandare l'header Authorization. Per
//! questo la finestra carica http://127.0.0.1:<porta>/<segreto>/ e il proxy
//! inoltra al server aggiungendo le credenziali, sia per HTTP sia per /ws.
//! Il segreto casuale nel percorso impedisce ad altri programmi locali di
//! usare la sessione attraverso la porta.

use axum::{
    body::Body,
    extract::{
        ws::{Message as AMsg, WebSocket, WebSocketUpgrade},
        Request, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Router,
};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as TMsg};
use url::Url;

const MAX_BODY: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct Upstream {
    base: Url,
    auth: HeaderValue,
}

impl Upstream {
    pub fn new(base: &str, username: &str, password: &str) -> Result<Self, String> {
        let base = Url::parse(base).map_err(|_| "indirizzo non valido".to_string())?;
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
        let mut auth = HeaderValue::from_str(&format!("Basic {token}")).map_err(|e| e.to_string())?;
        auth.set_sensitive(true);
        Ok(Self { base, auth })
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("client http")
}

/// Verifica indirizzo e credenziali chiedendo /token a ttyd.
pub async fn check(up: &Upstream) -> Result<(), String> {
    let url = up.base.join("token").map_err(|e| e.to_string())?;
    let resp = client()
        .get(url)
        .header(header::AUTHORIZATION, up.auth.clone())
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("server non raggiungibile: {e}"))?;
    match resp.status().as_u16() {
        200 => Ok(()),
        401 | 403 => Err("nome utente o password errati".into()),
        s => Err(format!("il server ha risposto {s}: è davvero un terminale pi/ttyd?")),
    }
}

struct Ctx {
    up: Upstream,
    prefix: String,
    http: reqwest::Client,
}

/// Avvia il proxy e restituisce l'indirizzo locale da aprire nella finestra.
pub async fn start(up: Upstream) -> Result<(String, tokio::task::JoinHandle<()>), String> {
    let secret = uuid::Uuid::new_v4().simple().to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let ctx = Arc::new(Ctx { up, prefix: format!("/{secret}/"), http: client() });
    let app = Router::new()
        .route(&format!("/{secret}/ws"), get(ws_handler))
        .fallback(http_handler)
        .with_state(ctx);
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((format!("http://127.0.0.1:{port}/{secret}/"), task))
}

// Intestazioni da non inoltrare: gestite dal proxy o legate alla singola connessione.
const SKIP_REQ: &[header::HeaderName] = &[
    header::HOST,
    header::CONNECTION,
    header::ACCEPT_ENCODING,
    header::AUTHORIZATION,
    header::ORIGIN,
    header::REFERER,
    header::CONTENT_LENGTH,
    header::TRANSFER_ENCODING,
];
const SKIP_RESP: &[header::HeaderName] = &[
    header::CONNECTION,
    header::TRANSFER_ENCODING,
    header::CONTENT_LENGTH,
    header::WWW_AUTHENTICATE,
    header::STRICT_TRANSPORT_SECURITY,
];

async fn http_handler(State(ctx): State<Arc<Ctx>>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    if path == ctx.prefix.trim_end_matches('/') {
        return Redirect::permanent(&ctx.prefix).into_response();
    }
    let Some(rest) = path.strip_prefix(&ctx.prefix) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(mut url) = ctx.up.base.join(rest) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    url.set_query(req.uri().query());

    let method = req.method().clone();
    let mut headers = HeaderMap::new();
    for (k, v) in req.headers() {
        if !SKIP_REQ.contains(k) {
            headers.append(k.clone(), v.clone());
        }
    }
    headers.insert(header::AUTHORIZATION, ctx.up.auth.clone());
    let Ok(body) = axum::body::to_bytes(req.into_body(), MAX_BODY).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };

    let resp = match ctx.http.request(method, url).headers(headers).body(body).send().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("server non raggiungibile: {e}")).into_response(),
    };
    let mut out = Response::builder().status(resp.status());
    for (k, v) in resp.headers() {
        if !SKIP_RESP.contains(k) {
            out = out.header(k, v);
        }
    }
    match resp.bytes().await {
        Ok(b) => out.body(Body::from(b)).unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response()),
        Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    }
}

async fn ws_handler(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    // il sottoprotocollo si chiede al server solo se lo chiede la finestra (ttyd usa "tty")
    let tty = headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.split(',').any(|p| p.trim() == "tty"));
    ws.protocols(["tty"]).on_upgrade(move |sock| async move {
        if let Err(e) = pump(sock, &ctx.up, tty).await {
            eprintln!("websocket: {e}");
        }
    })
}

async fn pump(sock: WebSocket, up: &Upstream, tty: bool) -> Result<(), String> {
    let mut url = up.base.join("ws").map_err(|e| e.to_string())?;
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme).map_err(|_| "schema non valido")?;
    let mut req = url.as_str().into_client_request().map_err(|e| e.to_string())?;
    req.headers_mut().insert(header::AUTHORIZATION, up.auth.clone());
    if tty {
        req.headers_mut().insert(header::SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static("tty"));
    }
    let (remote, _) = tokio_tungstenite::connect_async(req).await.map_err(|e| e.to_string())?;

    let (mut up_tx, mut up_rx) = remote.split();
    let (mut dn_tx, mut dn_rx) = sock.split();

    let to_server = async {
        while let Some(Ok(m)) = dn_rx.next().await {
            let m = match m {
                AMsg::Text(t) => TMsg::Text(t.to_string().into()),
                AMsg::Binary(b) => TMsg::Binary(b.to_vec().into()),
                AMsg::Ping(_) | AMsg::Pong(_) => continue,
                AMsg::Close(_) => break,
            };
            if up_tx.send(m).await.is_err() {
                break;
            }
        }
        let _ = up_tx.close().await;
    };
    let to_window = async {
        while let Some(Ok(m)) = up_rx.next().await {
            let m = match m {
                TMsg::Text(t) => AMsg::Text(t.to_string().into()),
                TMsg::Binary(b) => AMsg::Binary(b.to_vec().into()),
                TMsg::Close(_) => break,
                TMsg::Ping(_) | TMsg::Pong(_) | TMsg::Frame(_) => continue,
            };
            if dn_tx.send(m).await.is_err() {
                break;
            }
        }
        let _ = dn_tx.close().await;
    };
    // quando un lato chiude, si chiude anche l'altro
    let (tx_close, rx_close) = tokio::sync::oneshot::channel::<()>();
    let to_server = async move {
        tokio::select! {
            _ = to_server => {},
            _ = rx_close => {},
        }
    };
    let to_window = async move {
        to_window.await;
        let _ = tx_close.send(());
    };
    tokio::join!(to_server, to_window);
    Ok(())
}
