//! The desktop app's only way into Talyvor: tools on app.talyvor.com/mcp, called with the Connect token.
//! Every tool there prepares and never pays, so nothing here can move money or approve anything.

use crate::connect::{self, Saved, Session};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Instant;
use tokio::sync::Mutex;

const PROTOCOL: &str = "2025-11-25";

pub struct Talyvor {
    pub base: String,
    pub http: reqwest::Client,
    session: Mutex<Option<Session>>,
    saved: Mutex<Option<Saved>>,
    /// Off only in tests, which must not write to the computer's keychain.
    keychain: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Approval {
    pub id: String,
    pub title: String,
    pub amount_minor: i64,
    pub currency: String,
    pub approval_url: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Reply {
    pub reply: String,
    pub approval_url: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Turn {
    pub role: String,
    pub content: String,
}

impl Talyvor {
    /// `saved` is what the keychain held, if it was issued by this `base`.
    pub fn new(base: String, saved: Option<Saved>) -> Self {
        let saved = saved.filter(|s| s.base == base);
        Talyvor { base, http: reqwest::Client::new(), session: Mutex::new(None), saved: Mutex::new(saved), keychain: true }
    }

    pub fn in_memory(base: String) -> Self {
        Talyvor { keychain: false, ..Self::new(base, None) }
    }

    fn forget(&self) {
        if self.keychain {
            connect::forget();
        }
    }

    pub async fn signed_in(&self) -> bool {
        self.saved.lock().await.is_some()
    }

    pub async fn sign_in(&self, open: impl FnOnce(String)) -> Result<(), String> {
        let s = connect::sign_in(&self.http, &self.base, open).await?;
        self.keep(s).await
    }

    pub async fn sign_out(&self) {
        if let Some(saved) = self.saved.lock().await.take() {
            connect::revoke(&self.http, &saved).await;
        }
        *self.session.lock().await = None;
        self.forget();
    }

    async fn keep(&self, s: Session) -> Result<(), String> {
        let stored = if self.keychain { connect::store(&s.saved) } else { Ok(()) };
        *self.saved.lock().await = Some(s.saved.clone());
        *self.session.lock().await = Some(s);
        stored
    }

    async fn access_token(&self, renew: bool) -> Result<String, String> {
        if !renew {
            if let Some(s) = self.session.lock().await.as_ref().filter(|s| s.access_expires > Instant::now()) {
                return Ok(s.access_token.clone());
            }
        }
        let saved = self.saved.lock().await.clone().ok_or("Sign in to Talyvor first.")?;
        match connect::refresh(&self.http, &saved).await {
            Ok(s) => {
                let t = s.access_token.clone();
                self.keep(s).await?;
                Ok(t)
            }
            Err(e) => {
                // The grant was revoked (Connected apps on the web) or ran out: sign in again.
                *self.saved.lock().await = None;
                self.forget();
                Err(format!("Sign in to Talyvor again: {e}"))
            }
        }
    }

    /// One tools/call; the tool's structuredContent, or the words Talyvor refused it with.
    pub async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}});
        let mut renew = false;
        loop {
            let token = self.access_token(renew).await?;
            let r = self.http.post(connect::mcp_resource(&self.base))
                .bearer_auth(token)
                .header("MCP-Protocol-Version", PROTOCOL)
                .header("Accept", "application/json, text/event-stream")
                .json(&body)
                .send().await.map_err(|_| "Talyvor cannot be reached. Check the connection.".to_string())?;
            if r.status() == reqwest::StatusCode::UNAUTHORIZED && !renew {
                renew = true;
                continue;
            }
            let v: Value = r.json().await.map_err(|_| "Talyvor could not answer just now. Try again shortly.".to_string())?;
            return tool_result(v);
        }
    }

    pub async fn ask(&self, message: &str, thread: &[Turn]) -> Result<Reply, String> {
        let v = self.call("ask_taly", json!({"message": message, "thread": thread})).await?;
        serde_json::from_value(v).map_err(|_| "Taly could not answer just now. Try again shortly.".into())
    }

    pub async fn approvals(&self) -> Result<Vec<Approval>, String> {
        #[derive(Deserialize)]
        struct List {
            approvals: Vec<Approval>,
        }
        let v = self.call("list_approvals", json!({})).await?;
        Ok(serde_json::from_value::<List>(v).map_err(|e| e.to_string())?.approvals)
    }

    pub async fn transcribe(&self, audio_base64: &str, mime: &str) -> Result<String, String> {
        let v = self.call("transcribe", json!({"audio_base64": audio_base64, "mime": mime})).await?;
        v.get("text").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| "Nothing was heard. Try again.".into())
    }
}

fn tool_result(v: Value) -> Result<Value, String> {
    if let Some(e) = v.get("error") {
        return Err(e.get("message").and_then(Value::as_str).unwrap_or("Talyvor refused this.").to_owned());
    }
    let r = v.get("result").ok_or("Talyvor could not answer just now. Try again shortly.")?;
    if r.get("isError").and_then(Value::as_bool) == Some(true) {
        let words = r.pointer("/content/0/text").and_then(Value::as_str).unwrap_or("Talyvor refused this.");
        return Err(words.to_owned());
    }
    r.get("structuredContent").cloned().ok_or_else(|| "Talyvor could not answer just now. Try again shortly.".into())
}

/// The approvals in `now` that were not in `seen`: each is notified once.
pub fn new_approvals<'a>(seen: &std::collections::HashSet<String>, now: &'a [Approval]) -> Vec<&'a Approval> {
    now.iter().filter(|a| !seen.contains(&a.id)).collect()
}

/// An amount in minor units, as the person reads it: "£120.00", "1,250.00 USDC".
pub fn money(minor: i64, currency: &str) -> String {
    let (sign, minor) = if minor < 0 { ("-", minor.unsigned_abs()) } else { ("", minor as u64) };
    let whole = (minor / 100).to_string();
    let mut grouped = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let n = format!("{grouped}.{:02}", minor % 100);
    match currency {
        "GBP" => format!("{sign}£{n}"),
        "EUR" => format!("{sign}€{n}"),
        "USD" => format!("{sign}${n}"),
        c => format!("{sign}{n} {c}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_reads_like_the_web_app() {
        assert_eq!(money(12000, "GBP"), "£120.00");
        assert_eq!(money(125000, "USDC"), "1,250.00 USDC");
        assert_eq!(money(-5, "EUR"), "-€0.05");
    }

    #[test]
    fn an_approval_is_notified_once() {
        let a = |id: &str| Approval { id: id.into(), title: "ACME".into(), amount_minor: 1, currency: "GBP".into(), approval_url: String::new() };
        let seen: std::collections::HashSet<String> = ["r1".to_string()].into();
        let now = [a("r1"), a("r2")];
        assert_eq!(new_approvals(&seen, &now), vec![&now[1]]);
    }

    #[test]
    fn a_refused_tool_says_why() {
        let v = json!({"jsonrpc": "2.0", "id": 1, "result": {"isError": true, "content": [{"type": "text", "text": "Over the limit."}]}});
        assert_eq!(tool_result(v), Err("Over the limit.".into()));
        assert_eq!(tool_result(json!({"error": {"code": -32602, "message": "Unknown tool"}})), Err("Unknown tool".into()));
    }
}
