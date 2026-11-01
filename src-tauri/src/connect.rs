//! Signing in with Talyvor Connect (talyvor-suite apps/bff/oauth.go): the desktop app registers itself
//! as a public client (RFC 7591), runs the code flow with PKCE S256 through the person's own browser,
//! catches the answer on a loopback address (RFC 8252) and checks RFC 9207's `iss`. The refresh token
//! lives in the system keychain (macOS Keychain, Windows Credential Manager), never in a file.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

pub const SCOPES: &str = "wallets:read invoices:draft payments:prepare";
const KEYCHAIN_SERVICE: &str = "com.talyvor.desktop";
const KEYCHAIN_USER: &str = "talyvor-connect";
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);

/// What survives a restart: who issued the client, the client and its latest refresh token.
#[derive(Clone, Serialize, Deserialize)]
pub struct Saved {
    pub base: String,
    pub client_id: String,
    pub refresh_token: String,
}

pub struct Session {
    pub saved: Saved,
    pub access_token: String,
    pub access_expires: Instant,
}

#[derive(Deserialize)]
struct TokenAnswer {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

fn random_token() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// The code from the loopback request line, once its state and issuer are the ones this sign-in expects.
pub fn code_from_callback(request_line: &str, state: &str, issuer: &str) -> Result<String, String> {
    let target = request_line.split_whitespace().nth(1).ok_or("not an HTTP request")?;
    let u = url::Url::parse(&format!("http://127.0.0.1{target}")).map_err(|e| e.to_string())?;
    if u.path() != "/callback" {
        return Err("not the sign-in callback".into());
    }
    let q = |k: &str| u.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned());
    if let Some(e) = q("error") {
        return Err(q("error_description").unwrap_or(e));
    }
    if q("state").as_deref() != Some(state) {
        return Err("the sign-in answer does not belong to this sign-in".into());
    }
    if q("iss").as_deref() != Some(issuer) {
        return Err("the sign-in answer came from another server".into());
    }
    q("code").ok_or_else(|| "the sign-in answer has no code".into())
}

pub fn mcp_resource(base: &str) -> String {
    format!("{base}/mcp")
}

/// Signs in: `open` is handed the authorization URL to show in the person's browser.
pub async fn sign_in(http: &reqwest::Client, base: &str, open: impl FnOnce(String)) -> Result<Session, String> {
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let redirect = format!("http://127.0.0.1:{}/callback", listener.local_addr().map_err(|e| e.to_string())?.port());

    #[derive(Deserialize)]
    struct Registered {
        client_id: String,
    }
    let reg: Registered = http
        .post(format!("{base}/oauth/register"))
        .json(&serde_json::json!({"redirect_uris": [redirect], "client_name": "Talyvor for desktop"}))
        .send().await.map_err(|e| e.to_string())?
        .error_for_status().map_err(|e| e.to_string())?
        .json().await.map_err(|e| e.to_string())?;

    let (verifier, state) = (random_token(), random_token());
    let mut auth = url::Url::parse(&format!("{base}/oauth/authorize")).map_err(|e| e.to_string())?;
    auth.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &reg.client_id)
        .append_pair("redirect_uri", &redirect)
        .append_pair("scope", SCOPES)
        .append_pair("state", &state)
        .append_pair("code_challenge", &pkce_challenge(&verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("resource", &mcp_resource(base));
    open(auth.into());

    let code = tokio::time::timeout(SIGN_IN_WAIT, async {
        loop {
            let (sock, _) = listener.accept().await.map_err(|e| e.to_string())?;
            // The request line can arrive in several pieces; read up to its end, 8 KiB at most.
            let mut sock = BufReader::new(sock);
            let mut line = String::new();
            let _ = (&mut sock).take(8192).read_line(&mut line).await;
            let got = code_from_callback(line.trim_end(), &state, base);
            let page = match &got {
                Ok(_) => "You are signed in to Talyvor. You can close this tab.".to_string(),
                Err(e) => format!("Talyvor could not sign you in: {}", html_escape(e)),
            };
            let body = format!("<!doctype html><meta charset=utf-8><title>Talyvor</title><body style=\"background:#060A12;color:#E6EEF7;font:15px/24px system-ui;display:grid;place-items:center;min-height:90vh\"><p>{page}</p>");
            let _ = sock.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
            match got {
                // A stray request (a favicon, another tab) is answered and the wait goes on.
                Err(e) if e == "not the sign-in callback" => continue,
                other => return other,
            }
        }
    })
    .await
    .map_err(|_| "the sign-in was not finished in the browser within five minutes".to_string())??;

    let t = token(http, base, &[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &redirect),
        ("client_id", &reg.client_id),
        ("code_verifier", &verifier),
        ("resource", &mcp_resource(base)),
    ]).await?;
    let s = session(Saved { base: base.into(), client_id: reg.client_id, refresh_token: t.refresh_token.clone() }, t);
    Ok(s)
}

/// Trades the saved refresh token for a new pair; the old refresh token is spent (Connect rotates them).
pub async fn refresh(http: &reqwest::Client, saved: &Saved) -> Result<Session, String> {
    let t = token(http, &saved.base, &[
        ("grant_type", "refresh_token"),
        ("refresh_token", &saved.refresh_token),
        ("client_id", &saved.client_id),
        ("resource", &mcp_resource(&saved.base)),
    ]).await?;
    Ok(session(Saved { refresh_token: t.refresh_token.clone(), ..saved.clone() }, t))
}

pub async fn revoke(http: &reqwest::Client, saved: &Saved) {
    let _ = http.post(format!("{}/oauth/revoke", saved.base))
        .form(&[("token", saved.refresh_token.as_str()), ("client_id", saved.client_id.as_str())])
        .send().await;
}

async fn token(http: &reqwest::Client, base: &str, form: &[(&str, &str)]) -> Result<TokenAnswer, String> {
    let r = http.post(format!("{base}/oauth/token")).form(form).send().await.map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        #[derive(Deserialize)]
        struct OAuthErr {
            error_description: Option<String>,
        }
        let e: Option<OAuthErr> = r.json().await.ok();
        return Err(e.and_then(|e| e.error_description).unwrap_or_else(|| "Talyvor refused the sign-in".into()));
    }
    r.json().await.map_err(|e| e.to_string())
}

fn session(saved: Saved, t: TokenAnswer) -> Session {
    // A minute early, so a token never runs out in the middle of a call.
    let life = Duration::from_secs(t.expires_in.saturating_sub(60));
    Session { saved, access_token: t.access_token, access_expires: Instant::now() + life }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER).map_err(|e| e.to_string())
}

pub fn load() -> Option<Saved> {
    serde_json::from_str(&entry().ok()?.get_password().ok()?).ok()
}

pub fn store(saved: &Saved) -> Result<(), String> {
    entry()?.set_password(&serde_json::to_string(saved).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

pub fn forget() {
    if let Ok(e) = entry() {
        let _ = e.delete_credential();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636_appendix_b() {
        assert_eq!(pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn callback_needs_this_sign_ins_state_and_issuer() {
        let base = "https://app.talyvor.com";
        let line = |q: &str| format!("GET /callback?{q} HTTP/1.1");
        let iss = "iss=https%3A%2F%2Fapp.talyvor.com";
        assert_eq!(code_from_callback(&line(&format!("code=c1&state=s1&{iss}")), "s1", base), Ok("c1".into()));
        assert!(code_from_callback(&line(&format!("code=c1&state=other&{iss}")), "s1", base).is_err());
        assert!(code_from_callback(&line("code=c1&state=s1&iss=https%3A%2F%2Fevil.example"), "s1", base).is_err());
        assert!(code_from_callback(&line("code=c1&state=s1"), "s1", base).is_err());
    }
}
