//! HTTP transport helpers over ureq 2.12.
//!
//! Centralised so the client, sync and cover layers share one codepath and
//! one error mapping.  No TLS: the device talks to the LAN pbemu-api over
//! plain http; TLS termination (if a remote Kavita needs it) lives in the
//! Python server.

use std::io::Read;

use ureq::Agent;

const MAX_BODY: u64 = 16 * 1024 * 1024; // cover PNGs can be a few MB

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// GET and return the body as UTF-8 text.
pub fn get_text(agent: &Agent, base: &str, path: &str, token: &str) -> Result<String, String> {
    let mut text = String::new();
    agent
        .get(&format!("{base}{path}"))
        .set("Authorization", &bearer(token))
        .call()
        .map_err(|e| ureq_err(&e))?
        .into_reader()
        .take(MAX_BODY)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    Ok(text)
}

/// GET and return the raw body bytes.
pub fn get_bytes(agent: &Agent, base: &str, path: &str, token: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    agent
        .get(&format!("{base}{path}"))
        .set("Authorization", &bearer(token))
        .call()
        .map_err(|e| ureq_err(&e))?
        .into_reader()
        .take(MAX_BODY)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// POST a JSON string body, return the response text.
pub fn post_text(
    agent: &Agent,
    base: &str,
    path: &str,
    body: &str,
    token: &str,
) -> Result<String, String> {
    let mut text = String::new();
    agent
        .post(&format!("{base}{path}"))
        .set("Authorization", &bearer(token))
        .set("Content-Type", "application/json")
        .send_string(body)
        .map_err(|e| ureq_err(&e))?
        .into_reader()
        .take(MAX_BODY)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    Ok(text)
}

/// DELETE a resource, return the response text (empty body tolerated).
pub fn delete_text(agent: &Agent, base: &str, path: &str, token: &str) -> Result<String, String> {
    let mut text = String::new();
    agent
        .delete(&format!("{base}{path}"))
        .set("Authorization", &bearer(token))
        .call()
        .map_err(|e| ureq_err(&e))?
        .into_reader()
        .take(MAX_BODY)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    Ok(text)
}

/// Render a ureq error (transport or HTTP status) as a message.
fn ureq_err(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => format!("HTTP {} {}", code, resp.status_text()),
        ureq::Error::Transport(t) => t.to_string(),
    }
}
