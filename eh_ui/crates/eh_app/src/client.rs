//! Typed HTTP client for the pbemu-api REST surface.
//!
//! Talks to the provider-neutral `/api/v1` endpoints (the same contract the
//! C app used): list/`sync/delta`/`sync/state`/cover/file.  Maps the
//! `BookMeta` JSON shape into a Rust struct.

use serde::{Deserialize, Deserializer};

fn deserialize_string_or_number<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StrOrNum {
        Str(String),
        Num(i64),
        Float(f64),
    }
    match Option::<StrOrNum>::deserialize(deserializer)? {
        None => Ok(None),
        Some(StrOrNum::Str(s)) => Ok(Some(s)),
        Some(StrOrNum::Num(n)) => Ok(Some(n.to_string())),
        Some(StrOrNum::Float(f)) => Ok(Some((f as i64).to_string())),
    }
}
/// Author attribution helper.
pub fn author_first(authors: &[String]) -> &str {
    authors.first().map(|s| s.as_str()).unwrap_or("")
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookMeta {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub series: Option<String>,
    #[serde(default)]
    pub series_id: Option<String>,
    #[serde(default)]
    pub series_idx: Option<f64>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub cover: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    pub added_at: Option<String>,
    pub updated_at: Option<String>,
    #[serde(default)]
    pub remote_only: bool,
    /// Server-folded search blob (folded title + authors + series).
    /// Suggestions are folded server-side, so searches must match this
    /// folded text — a "songgong" suggestion from "sŏnggong" never
    /// matches the raw title.
    #[serde(default)]
    pub search_text: Option<String>,
    /// Suggestion term edges for prefix completion (server-generated).
    #[serde(default)]
    pub suggest: Vec<String>,
}

impl BookMeta {
    pub fn author(&self) -> &str {
        author_first(&self.authors)
    }
}

/// `POST /api/v1/sync/delta` response.  Server emits camelCase keys
/// (`nextCursor`, `serverTime`), matching the C app's parse.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delta {
    #[serde(default)]
    pub added: Vec<BookMeta>,
    #[serde(default)]
    pub updated: Vec<BookMeta>,
    #[serde(default)]
    pub removed: Vec<String>,
    #[serde(default)]
    pub next_cursor: i64,
    #[serde(default)]
    pub more: bool,
    #[serde(default)]
    pub server_time: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
}

/// A plain-HTTP client bound to one API base + bearer token.  No TLS: the
/// device talks to the LAN pbemu-api over http; TLS termination (if a remote
/// Kavita needs it) happens in the Python server.
#[derive(Clone)]
pub struct ApiClient {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl ApiClient {
    pub fn new(base: &str, token: &str) -> Self {
        let agent = ureq::AgentBuilder::new().build();
        Self {
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            agent,
        }
    }

    fn get(&self, path: &str) -> Result<String, String> {
        crate::util::get_text(&self.agent, &self.base, path, &self.token)
    }

    fn post(&self, path: &str, body: &str) -> Result<String, String> {
        crate::util::post_text(&self.agent, &self.base, path, body, &self.token)
    }

    /// `GET /api/v1/books?limit=N` → paginated `items` array.
    pub fn list_books(&self, limit: u32) -> Result<Vec<BookMeta>, String> {
        let text = self.get(&format!("/api/v1/books?limit={limit}"))?;
        #[derive(Deserialize)]
        struct Frame {
            #[serde(default)]
            items: Vec<BookMeta>,
        }
        let frame: Frame =
            serde_json::from_str(&text).map_err(|e| format!("bad book list json: {e}"))?;
        Ok(frame.items)
    }

    /// `POST /api/v1/sync/delta` — the metadata-only diff (cursor = last
    /// applied nextCursor, 0 = full first sync).
    pub fn delta(&self, cursor: i64, limit: u32) -> Result<Delta, String> {
        let body = format!("{{\"cursor\":{cursor},\"limit\":{limit}}}");
        let text = self.post("/api/v1/sync/delta", &body)?;
        serde_json::from_str(&text).map_err(|e| format!("bad delta json: {e}"))
    }

    /// `POST /api/v1/sync/state` — report device book ownership.  Fire-and-forget.
    pub fn report_state(&self, known: &[String]) -> Result<(), String> {
        let body = format!(
            "{{\"deviceId\":\"eh_rust\",\"known\":{}}}",
            serde_json::to_string(known).map_err(|e| e.to_string())?
        );
        let _ = self.post("/api/v1/sync/state", &body)?;
        Ok(())
    }

    /// `GET /api/v1/books/{id}/cover` → raw cover bytes (PNG).  Uses
    /// `?access_token=` since the device image path may not resend the
    /// Authorization header.
    pub fn cover(&self, id: &str) -> Result<Vec<u8>, String> {
        crate::util::get_bytes(
            &self.agent,
            &self.base,
            &format!("/api/v1/books/{}/cover?access_token={}", id, self.token),
            &self.token,
        )
    }

    /// `GET /api/v1/books/{id}/file` → the raw book file bytes.  Uses
    /// `?access_token=` like the cover path (the device image route may not
    /// resend the Authorization header).
    pub fn file(&self, id: &str) -> Result<Vec<u8>, String> {
        crate::util::get_bytes(
            &self.agent,
            &self.base,
            &format!("/api/v1/books/{}/file?access_token={}", id, self.token),
            &self.token,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_bookmeta() {
        let j = r#"{"id":"kavita_ch_1","title":"One Book","authors":["A"],"format":"epub"}"#;
        let b: BookMeta = serde_json::from_str(j).unwrap();
        assert_eq!(b.id, "kavita_ch_1");
        assert_eq!(b.author(), "A");
    }

    #[test]
    fn deserializes_delta() {
        let j = r#"{"added":[{"id":"x","title":"X"}],"removed":["y"],"nextCursor":5,"more":false}"#;
        let d: Delta = serde_json::from_str(j).unwrap();
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.removed, vec!["y"]);
        assert_eq!(d.next_cursor, 5);
    }
}
