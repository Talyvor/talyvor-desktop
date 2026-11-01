//! The desktop app against a local stand-in for app.talyvor.com's Connect endpoints (talyvor-suite
//! apps/bff/oauth.go, mcp.go): sign in with DCR + PKCE + a loopback redirect, chat with Taly, see the
//! approvals waiting, and keep working when the access token is refused and the refresh token rotates.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use talyvor_desktop_lib::{connect::pkce_challenge, mcp::Talyvor};

#[derive(Default)]
struct Seen {
    challenge: String,
    refused_once: bool,
    tools: Vec<String>,
}

fn form(body: &str) -> std::collections::HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes()).into_owned().collect()
}

fn stand_in(seen: Arc<Mutex<Seen>>) -> String {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body).unwrap();
            let bearer = req.headers().iter().find(|h| h.field.equiv("Authorization")).map(|h| h.value.to_string());
            let (status, answer) = match req.url() {
                "/oauth/register" => {
                    let v: Value = serde_json::from_str(&body).unwrap();
                    assert!(v["redirect_uris"][0].as_str().unwrap().starts_with("http://127.0.0.1:"));
                    (201, json!({"client_id": "tlv_oc_desk"}))
                }
                "/oauth/token" => {
                    let f = form(&body);
                    assert_eq!(f["client_id"], "tlv_oc_desk");
                    assert!(f["resource"].ends_with("/mcp"));
                    match f["grant_type"].as_str() {
                        "authorization_code" => {
                            assert_eq!(f["code"], "code-1");
                            assert_eq!(pkce_challenge(&f["code_verifier"]), seen.lock().unwrap().challenge, "PKCE verifier");
                            (200, json!({"access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600}))
                        }
                        "refresh_token" => {
                            assert_eq!(f["refresh_token"], "rt-1", "the latest refresh token");
                            (200, json!({"access_token": "at-2", "refresh_token": "rt-2", "expires_in": 3600}))
                        }
                        g => panic!("grant_type {g}"),
                    }
                }
                "/mcp" => {
                    let mut s = seen.lock().unwrap();
                    if !s.refused_once {
                        // The first access token is refused, as a revoked or restarted one would be.
                        s.refused_once = true;
                        assert_eq!(bearer.as_deref(), Some("Bearer at-1"));
                        (401, json!({"error": "sign in"}))
                    } else {
                        assert_eq!(bearer.as_deref(), Some("Bearer at-2"));
                        let v: Value = serde_json::from_str(&body).unwrap();
                        let tool = v["params"]["name"].as_str().unwrap().to_owned();
                        s.tools.push(tool.clone());
                        let out = match tool.as_str() {
                            "ask_taly" => {
                                assert_eq!(v["params"]["arguments"]["message"], "pay the ACME invoice");
                                json!({"reply": "I prepared £120.00 to ACME Ltd. It waits for your approval.", "approval_url": "https://app.talyvor.com/approve/pr_1"})
                            }
                            "list_approvals" => json!({"approvals": [{"id": "pr_1", "title": "ACME Ltd", "amount_minor": 12000, "currency": "GBP", "approval_url": "https://app.talyvor.com/approve/pr_1"}]}),
                            t => panic!("tool {t}"),
                        };
                        (200, json!({"jsonrpc": "2.0", "id": v["id"], "result": {"content": [{"type": "text", "text": out.to_string()}], "structuredContent": out, "isError": false}}))
                    }
                }
                u => panic!("unexpected {u}"),
            };
            let _ = req.respond(tiny_http::Response::from_string(answer.to_string()).with_status_code(status));
        }
    });
    base
}

/// The person's browser: consents at once and is sent back to the app's loopback address.
fn browser(base: String, seen: Arc<Mutex<Seen>>) -> impl FnOnce(String) {
    move |authorize: String| {
        let u = url::Url::parse(&authorize).unwrap();
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["scope"], "wallets:read invoices:draft payments:prepare");
        seen.lock().unwrap().challenge = q["code_challenge"].clone();
        let back = url::Url::parse_with_params(&q["redirect_uri"], &[("code", "code-1"), ("state", &q["state"]), ("iss", &base)]).unwrap();
        std::thread::spawn(move || {
            let mut s = std::net::TcpStream::connect(back.socket_addrs(|| None).unwrap()[0]).unwrap();
            write!(s, "GET {}?{} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n", back.path(), back.query().unwrap()).unwrap();
            let mut page = String::new();
            let _ = s.read_to_string(&mut page);
        });
    }
}

#[tokio::test]
async fn signs_in_chats_and_sees_the_approvals_waiting() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let base = stand_in(seen.clone());
    let t = Talyvor::in_memory(base.clone());
    assert!(!t.signed_in().await);

    t.sign_in(browser(base.clone(), seen.clone())).await.expect("sign in");
    assert!(t.signed_in().await);

    let r = t.ask("pay the ACME invoice", &[]).await.expect("Taly answers");
    assert!(r.reply.contains("waits for your approval"));
    assert_eq!(r.approval_url.as_deref(), Some("https://app.talyvor.com/approve/pr_1"));

    let list = t.approvals().await.expect("approvals");
    assert_eq!((list[0].id.as_str(), list[0].amount_minor, list[0].currency.as_str()), ("pr_1", 12000, "GBP"));
    assert_eq!(seen.lock().unwrap().tools, ["ask_taly", "list_approvals"]);
}

#[tokio::test]
async fn a_sign_in_answer_from_another_server_is_refused() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let base = stand_in(seen.clone());
    let t = Talyvor::in_memory(base.clone());
    let err = t.sign_in(browser("https://evil.example".into(), seen)).await.err().expect("refused");
    assert!(err.contains("another server"), "{err}");
    assert!(!t.signed_in().await);
}
