//! Spotify authorization and a shared, serialized refresh-token client.
//! Tokens never enter the React UI or settings file.
pub mod library;
mod oauth;

use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State as TauriState};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Sign in to Spotify again.")]
    Unauthorized,
    #[error("Spotify request failed (HTTP {0}).")]
    Http(u16),
    #[error("{0}")]
    Protocol(String),
    #[error("Could not reach Spotify. Check your connection.")]
    Network(#[from] reqwest::Error),
    #[error("Could not store Spotify credentials: {0}")]
    Store(#[from] crate::auth::AuthError),
}
impl Error {
    pub fn needs_auth(&self) -> bool {
        matches!(self, Self::Unauthorized)
    }
}

fn api_failure(status: u16, path: &str) -> Error {
    if status == 403 && path == "/v1/me" {
        return Error::Protocol(
            "Spotify authorized sign-in but denied profile access (HTTP 403). For a development-mode app, its owner must have active Spotify Premium, and the signed-in account must be listed under User Management in that app's developer dashboard. Check these requirements for the Client ID entered here, then reconnect.".into(),
        );
    }
    Error::Http(status)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub client_id: String,
    pub user_id: String,
    pub display_name: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
}

pub struct Client {
    http: reqwest::Client,
    session: tokio::sync::Mutex<Session>,
    /// Serializes persistence with revocation, so an old refresh cannot restore a signed-out account.
    active: Mutex<bool>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn http_client() -> Result<reqwest::Client, Error> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
fn api_url(path: &str) -> Result<url::Url, Error> {
    if path.starts_with("//") {
        return Err(Error::Protocol("Invalid Spotify API path".into()));
    }
    let raw = if path.starts_with('/') {
        format!("https://api.spotify.com/v1{path}")
    } else {
        path.to_string()
    };
    let url =
        url::Url::parse(&raw).map_err(|_| Error::Protocol("Invalid Spotify API URL".into()))?;
    if url.scheme() != "https"
        || url.host_str() != Some("api.spotify.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || !url.path().starts_with("/v1/")
    {
        return Err(Error::Protocol("Refusing a non-Spotify API URL".into()));
    }
    Ok(url)
}
fn updated_session(old: &Session, token: TokenResponse) -> Result<Session, Error> {
    if token.access_token.is_empty() || token.expires_in == 0 {
        return Err(Error::Protocol(
            "Spotify returned an incomplete token".into(),
        ));
    }
    let refresh = token
        .refresh_token
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| old.refresh_token.clone());
    if refresh.is_empty() {
        return Err(Error::Unauthorized);
    }
    Ok(Session {
        access_token: token.access_token,
        refresh_token: refresh,
        expires_at: now().saturating_add(token.expires_in),
        ..old.clone()
    })
}

impl Client {
    pub fn new(session: Session) -> Result<Self, Error> {
        Ok(Self {
            http: http_client()?,
            session: tokio::sync::Mutex::new(session),
            active: Mutex::new(true),
        })
    }
    pub fn revoke(&self) {
        *self.active.lock().expect("spotify active") = false;
    }
    pub async fn token(&self) -> Result<String, Error> {
        let mut session = self.session.lock().await;
        if !*self.active.lock().expect("spotify active") {
            return Err(Error::Unauthorized);
        }
        if session.expires_at > now().saturating_add(60) {
            return Ok(session.access_token.clone());
        }
        let response = self
            .http
            .post("https://accounts.spotify.com/api/token")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &session.refresh_token),
                ("client_id", &session.client_id),
            ])
            .send()
            .await?;
        if matches!(response.status().as_u16(), 400 | 401) {
            return Err(Error::Unauthorized);
        }
        if !response.status().is_success() {
            return Err(Error::Http(response.status().as_u16()));
        }
        let token: TokenResponse = response.json().await?;
        let next = updated_session(&session, token)?;
        let active = self.active.lock().expect("spotify active");
        if !*active {
            return Err(Error::Unauthorized);
        }
        crate::auth::save_spotify(&next)?;
        *session = next;
        Ok(session.access_token.clone())
    }
    async fn expire(&self, rejected: &str) {
        let mut session = self.session.lock().await;
        if session.access_token == rejected {
            session.expires_at = 0;
        }
    }
    pub async fn get(&self, path: &str) -> Result<Value, Error> {
        self.request(reqwest::Method::GET, path, None).await
    }
    pub(super) async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, Error> {
        let url = api_url(path)?;
        for attempt in 0..3 {
            let token = self.token().await?;
            let mut request = self
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(&token);
            if let Some(ref body) = body {
                request = request.json(body);
            }
            let response = request.send().await?;
            let status = response.status();
            if status.as_u16() == 401 && attempt == 0 {
                self.expire(&token).await;
                continue;
            }
            // Never blindly retry playback mutations: a timed-out skip may already have happened.
            if status.as_u16() == 429 && method == reqwest::Method::GET && attempt < 2 {
                let delay = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1);
                if delay <= 30 {
                    tokio::time::sleep(Duration::from_secs(delay.max(1))).await;
                    continue;
                }
            }
            if status.as_u16() == 401 {
                return Err(Error::Unauthorized);
            }
            if !status.is_success() {
                return Err(api_failure(status.as_u16(), url.path()));
            }
            if status.as_u16() == 204 {
                return Ok(Value::Null);
            }
            let bytes = response.bytes().await?;
            if bytes.is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_slice(&bytes)
                .map_err(|_| Error::Protocol("Spotify sent an invalid response".into()));
        }
        Err(Error::Unauthorized)
    }
}

#[derive(Default)]
pub struct State {
    /// Serializes sign-in finalization, cancellation and credential deletion.
    lifecycle: Mutex<()>,
    pub client: Mutex<Option<Arc<Client>>>,
    pub identity: Mutex<Option<String>>,
    login: AtomicU64,
}
#[derive(Clone, Serialize)]
pub struct Status {
    pub configured: bool,
    pub authenticated: bool,
    pub display_name: Option<String>,
    pub playback_supported: bool,
}

pub fn client(app: &AppHandle) -> Result<Arc<Client>, String> {
    restore(app)?;
    app.state::<AppState>()
        .spotify
        .client
        .lock()
        .expect("spotify client")
        .clone()
        .ok_or_else(|| "Connect Spotify metadata access under Import first.".into())
}
pub fn status(state: &AppState) -> Status {
    let id = state
        .settings
        .lock()
        .expect("settings mutex")
        .spotify
        .client_id
        .clone();
    Status {
        configured: oauth::valid_client_id(&id),
        authenticated: state
            .spotify
            .client
            .lock()
            .expect("spotify client")
            .is_some(),
        display_name: state
            .spotify
            .identity
            .lock()
            .expect("spotify identity")
            .clone(),
        playback_supported: false,
    }
}
fn emit_status(app: &AppHandle) {
    let _ = app.emit("spotify://changed", status(&app.state::<AppState>()));
}

#[tauri::command]
pub fn spotify_status(app: AppHandle, state: TauriState<'_, AppState>) -> Status {
    if let Err(error) = restore(&app) {
        tracing::warn!(%error, "could not restore Spotify metadata session");
    }
    status(&state)
}

/// Metadata sessions are independent of the active playback source.
fn stop_locked(app: &AppHandle) {
    let state = app.state::<AppState>();
    state.spotify.login.fetch_add(1, Ordering::SeqCst);
    if let Some(client) = state.spotify.client.lock().expect("spotify client").take() {
        client.revoke();
    }
    state
        .spotify
        .identity
        .lock()
        .expect("spotify identity")
        .take();
}

pub fn restore(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let selected = state.settings.lock().expect("settings mutex");
    let _lifecycle = state.spotify.lifecycle.lock().expect("spotify lifecycle");
    if state
        .spotify
        .client
        .lock()
        .expect("spotify client")
        .is_some()
    {
        return Ok(());
    }
    let session = crate::auth::load_spotify()
        .map_err(|e| e.to_string())?
        .filter(|s| s.client_id == selected.spotify.client_id && !s.refresh_token.is_empty());
    if let Some(session) = session {
        *state.spotify.identity.lock().expect("spotify identity") =
            Some(session.display_name.clone());
        *state.spotify.client.lock().expect("spotify client") =
            Some(Arc::new(Client::new(session).map_err(|e| e.to_string())?));
    }
    Ok(())
}

#[tauri::command]
pub async fn spotify_disconnect(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let _lifecycle = state.spotify.lifecycle.lock().expect("spotify lifecycle");
    stop_locked(&app);
    crate::auth::clear_spotify().map_err(|e| e.to_string())?;
    drop(_lifecycle);
    emit_status(&app);
    Ok(())
}

fn same_login(app: &AppHandle, attempt: u64) -> bool {
    app.state::<AppState>().spotify.login.load(Ordering::SeqCst) == attempt
}
async fn read_request_line(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    tokio::io::BufReader::new(stream.take(8192))
        .read_until(b'\n', &mut bytes)
        .await?;
    if !bytes.ends_with(b"\n") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Incomplete callback request",
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).trim_end().to_owned())
}
async fn await_code(
    app: &AppHandle,
    listener: tokio::net::TcpListener,
    expected: &str,
    attempt: u64,
) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        if !same_login(app, attempt) {
            return Err("Spotify sign-in was cancelled.".into());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("Spotify sign-in timed out. Try connecting again.".into());
        }
        let accepted = tokio::time::timeout(Duration::from_secs(1), listener.accept()).await;
        let Ok(Ok((mut socket, _))) = accepted else {
            continue;
        };
        // A stalled local client cannot hold the callback port indefinitely.
        let Ok(Ok(line)) =
            tokio::time::timeout(Duration::from_secs(2), read_request_line(&mut socket)).await
        else {
            continue;
        };
        let parts: Vec<_> = line.split_whitespace().collect();
        let parsed = if parts.len() == 3 && parts[0] == "GET" {
            oauth::callback_code(parts[1], expected)
        } else {
            Err("Invalid callback request".into())
        };
        let (code, message) = match &parsed {
            Ok(Some(_)) => (
                "200 OK",
                "Spotify authorization received. Return to capsule while it checks account access. Connection is not complete until capsule confirms it.",
            ),
            Ok(None) => ("404 Not Found", "Not found."),
            Err(_) => (
                "400 Bad Request",
                "This sign-in could not be accepted. Return to capsule and try again.",
            ),
        };
        let response = format!("HTTP/1.1 {code}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{message}", message.len());
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            socket.write_all(response.as_bytes()),
        )
        .await;
        match parsed {
            Ok(Some(code)) => return Ok(code),
            Err(e) if e.contains("cancelled") => return Err(e),
            _ => continue, // Random local requests must not cancel a real login.
        }
    }
}

fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use windows::core::PCWSTR;
        use windows::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
        let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
        let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
        // ShellExecute opens a URL directly, without a command shell.
        let result = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(verb.as_ptr()),
                PCWSTR(wide.as_ptr()),
                None,
                None,
                SW_SHOWNORMAL,
            )
        };
        if result.0 as isize <= 32 {
            return Err("Could not open your browser.".into());
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|_| "Could not open your browser.".into())
    }
}

#[tauri::command]
pub async fn spotify_connect(app: AppHandle, client_id: String) -> Result<(), String> {
    let id = client_id.trim().to_string();
    if !oauth::valid_client_id(&id) {
        return Err("Enter the 32-character Client ID from your Spotify developer app.".into());
    }
    let state = app.state::<AppState>();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:43821")
        .await
        .map_err(|_| {
            "Spotify's sign-in port (43821) is in use. Finish the other sign-in and try again."
        })?;
    let verifier = oauth::random_secret()?;
    let csrf = oauth::random_secret()?;
    let attempt = state.spotify.login.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut settings = state.settings.lock().expect("settings mutex");

        let mut next = settings.clone();
        next.spotify.client_id = id.clone();
        let dir = state
            .data_dir
            .lock()
            .expect("data dir mutex")
            .clone()
            .ok_or("No data directory")?;
        crate::settings::save(&dir, &next).map_err(|e| e.to_string())?;
        *settings = next;
    }
    open_browser(&oauth::authorize_url(&id, &csrf, &verifier))?;
    let code = await_code(&app, listener, &csrf, attempt).await?;
    let http = http_client().map_err(|e| e.to_string())?;
    let response = http
        .post("https://accounts.spotify.com/api/token")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &id),
            ("redirect_uri", oauth::REDIRECT_URI),
            ("code_verifier", &verifier),
        ])
        .send()
        .await
        .map_err(|_| "Could not reach Spotify to finish sign-in.")?;
    if !response.status().is_success() {
        return Err(
            "Spotify could not complete sign-in. Check the Client ID and registered redirect URI."
                .into(),
        );
    }
    let tokens: TokenResponse = response
        .json()
        .await
        .map_err(|_| "Spotify returned an invalid token response.")?;
    let initial = Session {
        client_id: id,
        user_id: String::new(),
        display_name: String::new(),
        access_token: String::new(),
        refresh_token: String::new(),
        expires_at: 0,
    };
    let session = updated_session(&initial, tokens).map_err(|e| e.to_string())?;
    let new_client = Arc::new(Client::new(session).map_err(|e| e.to_string())?);
    let profile = new_client.get("/me").await.map_err(|e| e.to_string())?;
    let user_id = profile["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or("Spotify did not return an account ID.")?;
    let display_name = profile["display_name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(user_id);
    let session = {
        let mut session = new_client.session.lock().await;
        session.user_id = user_id.into();
        session.display_name = display_name.into();
        session.clone()
    };
    // Persist only metadata authorization; playback and its library stay untouched.
    let selected = state.settings.lock().expect("settings mutex");
    let lifecycle = state.spotify.lifecycle.lock().expect("spotify lifecycle");
    if selected.spotify.client_id != session.client_id
        || state.spotify.login.load(Ordering::SeqCst) != attempt
    {
        new_client.revoke();
        return Err("Spotify sign-in was cancelled.".into());
    }
    // Stop a concurrent refresh from overwriting the new account's credentials.
    if let Some(old) = state
        .spotify
        .client
        .lock()
        .expect("spotify client")
        .as_ref()
    {
        old.revoke();
    }
    crate::auth::save_spotify(&session).map_err(|e| e.to_string())?;
    stop_locked(&app);
    *state.spotify.client.lock().expect("spotify client") = Some(new_client);
    *state.spotify.identity.lock().expect("spotify identity") = Some(display_name.into());
    drop(lifecycle);
    drop(selected);
    emit_status(&app);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_forbidden_explains_why_browser_authorization_is_not_api_access() {
        let error = api_failure(403, "/v1/me");
        let message = error.to_string();
        assert!(message.contains("profile"));
        assert!(message.contains("Premium"));
        assert!(message.contains("User Management"));
        assert!(
            !error.needs_auth(),
            "repeated OAuth cannot grant blocked app access"
        );
        assert_eq!(
            api_failure(500, "/v1/me").to_string(),
            "Spotify request failed (HTTP 500)."
        );
    }
    #[tokio::test]
    async fn oauth_request_line_can_arrive_in_multiple_tcp_reads() {
        let (mut sender, mut receiver) = tokio::io::duplex(128);
        let send = tokio::spawn(async move {
            sender.write_all(b"GET /callback?code=abc").await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
            sender
                .write_all(b"&state=ours HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
                .await
                .unwrap();
        });
        let line = read_request_line(&mut receiver).await.unwrap();
        send.await.unwrap();
        assert_eq!(
            line.trim_end(),
            "GET /callback?code=abc&state=ours HTTP/1.1"
        );
    }
    #[test]
    fn bearer_tokens_are_never_sent_to_external_pagination_urls() {
        for url in [
            "https://evil.example/v1/me/tracks",
            "http://api.spotify.com/v1/me",
            "https://api.spotify.com.evil.example/v1/me",
            "https://user@api.spotify.com/v1/me",
            "//evil.example/me",
        ] {
            assert!(api_url(url).is_err(), "{url}");
        }
        assert_eq!(
            api_url("/me/tracks?limit=50").unwrap().as_str(),
            "https://api.spotify.com/v1/me/tracks?limit=50"
        );
        assert!(api_url("https://api.spotify.com/v1/me/tracks?offset=50").is_ok());
    }
    #[test]
    fn refreshing_preserves_an_omitted_refresh_token_and_accepts_rotation() {
        let old = Session {
            client_id: "client".into(),
            user_id: "user".into(),
            display_name: "Listener".into(),
            access_token: "expired".into(),
            refresh_token: "refresh".into(),
            expires_at: 0,
        };
        let next = updated_session(
            &old,
            TokenResponse {
                access_token: "new".into(),
                expires_in: 3600,
                refresh_token: None,
            },
        )
        .unwrap();
        assert_eq!(next.refresh_token, "refresh");
        assert_eq!(next.user_id, "user");
        assert!(next.expires_at > now());
        let rotated = updated_session(
            &old,
            TokenResponse {
                access_token: "new".into(),
                expires_in: 3600,
                refresh_token: Some("rotated".into()),
            },
        )
        .unwrap();
        assert_eq!(rotated.refresh_token, "rotated");
    }
}
