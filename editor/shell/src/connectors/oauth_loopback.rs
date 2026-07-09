//! A reusable OAuth implicit-flow loopback login (RFC 8252 native-app pattern). A connector
//! declares an [`OAuthLoopbackConfig`]; this module binds an ephemeral `127.0.0.1` listener,
//! opens the provider's authorize page in the system browser, and waits for the redirect.
//!
//! The implicit flow returns the token in the URL *fragment*, which a browser never sends to
//! the server. So the landing page is functional, not cosmetic: its inline JS reads
//! `location.hash` and `POST`s the token back to `/callback` on the same loopback origin. The
//! `state` is validated (CSRF), exactly one callback is accepted, then the listener closes and
//! the token is written to the keyring. Nothing here is provider-specific.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use super::Credentials;

/// What a connector must declare to use the loopback flow.
pub struct OAuthLoopbackConfig {
    pub authorize_url: String,
    /// Recorded for a future Authorization-Code store; unused by the implicit flow.
    #[allow(dead_code)]
    pub token_url: String,
    pub client_id: String,
    pub scope: String,
    /// `"token"` for implicit.
    pub response_type: String,
    /// Keyring account the token is stored under (the connector id).
    pub keyring_key: String,
}

/// An OAuth loopback failure.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("not configured: {0}")]
    NotConfigured(String),
    #[error("loopback listen failed: {0}")]
    Listen(String),
    #[error("could not open the browser: {0}")]
    Browser(String),
    #[error("state mismatch (possible CSRF)")]
    StateMismatch,
    #[error("login was denied or returned no token")]
    NoToken,
    #[error("login timed out")]
    Timeout,
    #[error("keyring error: {0}")]
    Keyring(String),
}

const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// Runs the full loopback login and stores the resulting token in the keyring.
pub fn run_loopback_login(config: &OAuthLoopbackConfig) -> Result<String, OAuthError> {
    if config.client_id.is_empty() {
        return Err(OAuthError::NotConfigured(format!(
            "{} OAuth client id (set the matching SAFFRON_*_CLIENT_ID env var)",
            config.keyring_key
        )));
    }

    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| OAuthError::Listen(e.to_string()))?;
    let addr = listener
        .local_addr()
        .map_err(|e| OAuthError::Listen(e.to_string()))?;
    // Loopback only — never a routable interface.
    if !addr.ip().is_loopback() {
        return Err(OAuthError::Listen(
            "bound a non-loopback address".to_owned(),
        ));
    }
    let redirect = format!("http://127.0.0.1:{}", addr.port());
    let state = csrf_state(addr.port());

    let auth_url = format!(
        "{}?response_type={}&client_id={}&redirect_uri={}&scope={}&state={}",
        config.authorize_url,
        enc(&config.response_type),
        enc(&config.client_id),
        enc(&redirect),
        enc(&config.scope),
        enc(&state),
    );
    crate::open_url_in_browser(&auth_url).map_err(OAuthError::Browser)?;

    listener
        .set_nonblocking(true)
        .map_err(|e| OAuthError::Listen(e.to_string()))?;
    let start = Instant::now();
    let mut token: Option<String> = None;
    while start.elapsed() < LOGIN_TIMEOUT {
        match listener.accept() {
            Ok((stream, _)) => match handle_conn(stream, &state)? {
                Some(t) => {
                    token = Some(t);
                    break;
                }
                None => continue,
            },
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(OAuthError::Listen(e.to_string())),
        }
    }
    let token = token.ok_or(OAuthError::Timeout)?;
    Credentials::global()
        .set_secret(&config.keyring_key, &token)
        .map_err(|e| OAuthError::Keyring(e.to_string()))?;
    Ok(token)
}

/// Handles one connection: serves the fragment-bridge page on the redirect `GET`, and on the
/// page's `POST /callback` validates `state` and returns the token.
fn handle_conn(mut stream: TcpStream, expected_state: &str) -> Result<Option<String>, OAuthError> {
    stream
        .set_nonblocking(false)
        .map_err(|e| OAuthError::Listen(e.to_string()))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();

    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    // Read until headers complete; for the POST, keep reading until the body is in.
    loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(done) = request_complete(&buf)
            && done
        {
            break;
        }
        if buf.len() > 64 * 1024 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");

    if method.eq_ignore_ascii_case("POST") && path.starts_with("/callback") {
        let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
        let token = form_field(body, "access_token");
        let state = form_field(body, "state");
        if state.as_deref() != Some(expected_state) {
            write_html(&mut stream, &page(false));
            return Err(OAuthError::StateMismatch);
        }
        match token {
            Some(t) if !t.is_empty() => {
                write_html(&mut stream, &page(true));
                Ok(Some(t))
            }
            _ => {
                write_html(&mut stream, &page(false));
                Err(OAuthError::NoToken)
            }
        }
    } else {
        // The redirect target: serve the bridge page; the token arrives on its POST.
        write_html(&mut stream, &page(true));
        Ok(None)
    }
}

/// Whether the buffer holds a complete request (headers + any declared body).
fn request_complete(buf: &[u8]) -> Option<bool> {
    let text = String::from_utf8_lossy(buf);
    let header_end = text.find("\r\n\r\n")?;
    let headers = &text[..header_end];
    let body_start = header_end + 4;
    let len = headers
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    Some(text.len() - body_start >= len)
}

fn form_field(body: &str, key: &str) -> Option<String> {
    body.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

fn write_html(stream: &mut TcpStream, html: &str) {
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

/// The Anima-styled, self-contained landing page (no external fetches). On the success
/// path its inline JS reads the URL fragment and POSTs the token back to `/callback`.
fn page(success: bool) -> String {
    let body = if success {
        r#"<div class="card"><div class="dot ok"></div><h1>Connecting…</h1><p id="msg">Finishing sign-in. You can close this tab and return to Saffron Anima.</p></div>
<script>
(function () {
  var h = (location.hash || "").replace(/^#/, "");
  var p = new URLSearchParams(h);
  var msg = document.getElementById("msg");
  if (p.get("error")) { msg.textContent = "Sign-in was cancelled. You can close this tab."; return; }
  var token = p.get("access_token");
  var state = p.get("state");
  if (!token) { msg.textContent = "No token received. You can close this tab."; return; }
  fetch("/callback", {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: "access_token=" + encodeURIComponent(token) + "&state=" + encodeURIComponent(state || "")
  }).then(function () { msg.textContent = "Connected. You can close this tab and return to Saffron Anima."; })
    .catch(function () { msg.textContent = "Connected. You can close this tab."; });
})();
</script>"#
    } else {
        r#"<div class="card"><div class="dot err"></div><h1>Sign-in failed</h1><p>The request could not be verified. Close this tab and try again from Saffron Anima.</p></div>"#
    };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Saffron Anima</title>
<style>
  html,body{{height:100%;margin:0}}
  body{{display:flex;align-items:center;justify-content:center;background:#0f1115;color:#e6e8ec;
    font:14px/1.5 system-ui,-apple-system,Segoe UI,Roboto,sans-serif}}
  .card{{max-width:380px;padding:32px;border:1px solid #232733;border-radius:12px;background:#161922;text-align:center}}
  h1{{font-size:18px;margin:12px 0 8px}}
  p{{color:#9aa3b2;margin:0}}
  .dot{{width:36px;height:36px;border-radius:50%;margin:0 auto}}
  .ok{{background:#f5a623}} .err{{background:#e5484d}}
</style></head><body>{body}</body></html>"#
    )
}

/// Percent-encodes a query-parameter value (unreserved set passes through).
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.replace('+', " ");
    let bytes = bytes.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An unpredictable single-use CSRF token for the loopback callback.
fn csrf_state(port: u16) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .hash(&mut h);
    std::process::id().hash(&mut h);
    port.hash(&mut h);
    format!("{:016x}{:016x}", h.finish(), {
        port.hash(&mut h);
        h.finish()
    })
}
