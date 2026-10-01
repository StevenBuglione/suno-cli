//! Interactive login in an isolated Chrome profile. This avoids depending on
//! Windows cookie decryption and never reads unrelated browser credentials.
use crate::{auth::BrowserAuth, errors::CliError};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio_tungstenite::tungstenite::Message;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn call(
    socket: &mut Socket,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value, CliError> {
    socket
        .send(Message::Text(
            json!({"id":id,"method":method,"params":params}).to_string(),
        ))
        .await
        .map_err(|_| CliError::Config("login browser connection failed".into()))?;
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(message) = socket.next().await {
            let message =
                message.map_err(|_| CliError::Config("login browser connection closed".into()))?;
            if let Message::Text(text) = message {
                let reply: Value = serde_json::from_str(&text)?;
                if reply["id"].as_u64() == Some(id) {
                    if reply.get("error").is_some() {
                        return Err(CliError::Config(format!("login browser rejected {method}")));
                    }
                    return Ok(reply["result"].clone());
                }
            }
        }
        Err(CliError::Config("login browser was closed".into()))
    })
    .await
    .map_err(|_| CliError::Config("login browser response timed out".into()))?
}

fn suno_domain(domain: &str) -> bool {
    matches!(domain.trim_start_matches('.'), "suno.com" | "auth.suno.com")
}

fn browser_auth(cookies: &[Value]) -> Option<BrowserAuth> {
    let relevant: Vec<&Value> = cookies
        .iter()
        .filter(|cookie| suno_domain(cookie["domain"].as_str().unwrap_or_default()))
        .collect();
    let client = relevant
        .iter()
        .filter(|cookie| {
            let name = cookie["name"].as_str().unwrap_or_default();
            (name == "__client" || name.starts_with("__client_"))
                && !cookie["value"].as_str().unwrap_or_default().is_empty()
        })
        .max_by_key(|cookie| {
            cookie["domain"]
                .as_str()
                .unwrap_or_default()
                .contains("auth.suno.com")
        })?;
    Some(BrowserAuth {
        clerk_client_cookie: client["value"].as_str()?.to_owned(),
        cookie_header: relevant
            .iter()
            .filter_map(|cookie| {
                Some(format!(
                    "{}={}",
                    cookie["name"].as_str()?,
                    cookie["value"].as_str()?
                ))
            })
            .collect::<Vec<_>>()
            .join("; "),
        device_id: relevant
            .iter()
            .find(|cookie| cookie["name"] == "ajs_anonymous_id")
            .and_then(|cookie| cookie["value"].as_str())
            .and_then(crate::auth::sanitize_device_id),
    })
}

pub async fn login(timeout_secs: u64) -> Result<BrowserAuth, CliError> {
    let profile = tempfile::Builder::new().prefix("suno-login-").tempdir()?;
    let chrome = crate::captcha::locate_chrome()?;
    let mut child = tokio::process::Command::new(chrome)
        .arg(format!("--user-data-dir={}", profile.path().display()))
        .args([
            "--remote-debugging-address=127.0.0.1",
            "--remote-debugging-port=0",
            "--no-first-run",
            "--no-default-browser-check",
            "https://suno.com/create",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    eprintln!(
        "Sign in to Suno in the separate Chrome window. Waiting up to {timeout_secs} seconds; the window closes after the session is captured."
    );
    let http = crate::auth::http_client()?;
    let result = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        let mut socket = None;
        let mut id = 0;
        loop {
            if child.try_wait()?.is_some() {
                return Err(CliError::Config("login browser was closed before sign-in completed".into()));
            }
            if socket.is_none()
                && let Ok(active) = tokio::fs::read_to_string(profile.path().join("DevToolsActivePort")).await
                && let Some(port) = active.lines().next().and_then(|line| line.parse::<u16>().ok())
                && let Ok(response) = http.get(format!("http://127.0.0.1:{port}/json/list")).send().await
                && let Ok(tabs) = response.json::<Vec<Value>>().await
                && let Some(url) = tabs.iter().filter(|tab| tab["type"] == "page")
                    .find_map(|tab| tab["webSocketDebuggerUrl"].as_str())
                && url.starts_with(&format!("ws://127.0.0.1:{port}/"))
                && let Ok((ws, _)) = tokio_tungstenite::connect_async(url).await
            {
                socket = Some(ws);
            }
            if let Some(ws) = socket.as_mut() {
                id += 1;
                let result = call(ws, id, "Network.getCookies", json!({"urls":["https://suno.com/","https://auth.suno.com/"]})).await?;
                if let Some(cookies) = result["cookies"].as_array()
                    && cookies.iter().any(|cookie| {
                        let name = cookie["name"].as_str().unwrap_or_default();
                        suno_domain(cookie["domain"].as_str().unwrap_or_default())
                            && (name == "__session" || name.starts_with("__session_"))
                            && cookie["value"].as_str().is_some_and(|value| value.split('.').count() == 3)
                    })
                    && let Some(auth) = browser_auth(cookies)
                    // Clerk sets a client cookie even before authentication.
                    // Only finish when an active session has been established.
                    && crate::auth::clerk_token_exchange(&http, &auth.clerk_client_cookie).await.is_ok()
                {
                    id += 1;
                    let _ = call(ws, id, "Browser.close", json!({})).await;
                    return Ok(auth);
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }).await.unwrap_or_else(|_| Err(CliError::Config("Suno browser login timed out; retry `suno auth --browser-login`".into())));
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_is_scoped_and_prefers_auth_cookie() {
        let cookies = vec![
            json!({"domain":"other.example","name":"__client","value":"unrelated-secret"}),
            json!({"domain":"evil-suno.com","name":"__client","value":"fake-secret"}),
            json!({"domain":".suno.com","name":"__client","value":"site-secret"}),
            json!({"domain":"auth.suno.com","name":"__client_suffix","value":"auth-secret"}),
        ];
        let auth = browser_auth(&cookies).unwrap();
        assert_eq!(auth.clerk_client_cookie, "auth-secret");
        assert!(!auth.cookie_header.contains("unrelated-secret"));
        assert!(!auth.cookie_header.contains("fake-secret"));
    }
}
