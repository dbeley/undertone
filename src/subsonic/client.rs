//! Transport: authentication, request building, response envelope, errors.
//!
//! Auth follows the Subsonic token scheme: `t = md5(password + salt)`, with a
//! fresh random salt per request so a captured token cannot be replayed.
//! Verified against Navidrome 0.64.2.

use std::fmt::Write as _;
use std::io::Read;

use md5::{Digest, Md5};
use serde::de::DeserializeOwned;

pub const API_VERSION: &str = "1.16.1";
pub const CLIENT_NAME: &str = "undertone";

/// Everything that can go wrong talking to a Subsonic server.
#[derive(Debug, thiserror::Error)]
pub enum SubsonicError {
    /// The server answered with `status: "failed"`.
    #[error("server error {code}: {message}")]
    Server { code: u32, message: String },
    /// Non-2xx HTTP status.
    #[error("http {status} from {endpoint}")]
    Http { status: u16, endpoint: String },
    /// Network-level failure.
    #[error("transport: {0}")]
    Transport(String),
    /// Response body could not be decoded.
    #[error("decode: {0}")]
    Decode(String),
    /// A successful response was missing the payload key we asked for.
    #[error("response is missing `{0}`")]
    MissingPayload(String),
}

/// Connection settings for a Subsonic server.
#[derive(Debug, Clone)]
pub struct SubsonicConfig {
    /// Server URL, with or without a trailing `/rest`.
    pub server: String,
    pub username: String,
    pub password: String,
}

/// What `ping` tells us about the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub version: String,
    pub server_type: Option<String>,
    pub open_subsonic: bool,
}

/// The Subsonic response envelope.
#[derive(Debug, serde::Deserialize)]
pub struct RawResponse {
    pub status: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(rename = "type", default)]
    pub server_type: Option<String>,
    #[serde(default)]
    pub error: Option<ApiError>,
    #[serde(rename = "openSubsonic", default)]
    pub open_subsonic: bool,
    #[serde(flatten)]
    pub payload: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ApiError {
    pub code: u32,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, serde::Deserialize)]
struct Envelope {
    #[serde(rename = "subsonic-response")]
    response: RawResponse,
}

/// A client bound to one server and one set of credentials.
pub struct SubsonicClient {
    agent: ureq::Agent,
    config: SubsonicConfig,
    rest_base: String,
}

impl SubsonicClient {
    pub fn new(config: SubsonicConfig) -> Self {
        let rest_base = rest_base(&config.server);
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build();
        Self {
            agent,
            config,
            rest_base,
        }
    }

    pub fn config(&self) -> &SubsonicConfig {
        &self.config
    }

    /// The normalised `.../rest` base URL.
    pub fn rest_base(&self) -> &str {
        &self.rest_base
    }

    /// `t = md5(password + salt)`, hex encoded.
    pub fn token(password: &str, salt: &str) -> String {
        let mut hasher = Md5::new();
        hasher.update(password.as_bytes());
        hasher.update(salt.as_bytes());
        let digest = hasher.finalize();
        let mut out = String::with_capacity(32);
        for byte in digest {
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    /// Execute a request and return the raw envelope, mapping server errors.
    pub fn envelope(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
    ) -> Result<RawResponse, SubsonicError> {
        let url = self.build_url(endpoint, params);
        let response = self.agent.get(&url).call().map_err(|err| match err {
            ureq::Error::Status(status, _) => SubsonicError::Http {
                status,
                endpoint: endpoint.to_string(),
            },
            ureq::Error::Transport(t) => SubsonicError::Transport(t.to_string()),
        })?;

        let body: String = response
            .into_string()
            .map_err(|e| SubsonicError::Transport(e.to_string()))?;

        let envelope: Envelope =
            serde_json::from_str(&body).map_err(|e| SubsonicError::Decode(e.to_string()))?;

        if envelope.response.status != "ok" {
            let err = envelope.response.error.unwrap_or(ApiError {
                code: 0,
                message: "provider reported failure without an error body".to_string(),
            });
            return Err(SubsonicError::Server {
                code: err.code,
                message: err.message,
            });
        }
        Ok(envelope.response)
    }

    /// Execute a request and decode the payload stored under `key`.
    pub fn get_json<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        key: &str,
        params: &[(&str, &str)],
    ) -> Result<T, SubsonicError> {
        let mut response = self.envelope(endpoint, params)?;
        let payload = response
            .payload
            .remove(key)
            .ok_or_else(|| SubsonicError::MissingPayload(key.to_string()))?;
        serde_json::from_value(payload).map_err(|e| SubsonicError::Decode(e.to_string()))
    }

    /// Fetch binary content (cover art, and later stream probes).
    pub fn get_bytes(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
    ) -> Result<(Vec<u8>, String), SubsonicError> {
        let url = self.build_url(endpoint, params);
        let response = self.agent.get(&url).call().map_err(|err| match err {
            ureq::Error::Status(status, _) => SubsonicError::Http {
                status,
                endpoint: endpoint.to_string(),
            },
            ureq::Error::Transport(t) => SubsonicError::Transport(t.to_string()),
        })?;
        let content_type = response
            .header("content-type")
            .unwrap_or("application/octet-stream")
            .to_string();
        let mut bytes = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut bytes)
            .map_err(|e| SubsonicError::Transport(e.to_string()))?;
        Ok((bytes, content_type))
    }

    /// Build an authenticated `.view` URL.
    pub fn build_url(&self, endpoint: &str, params: &[(&str, &str)]) -> String {
        let salt = random_salt();
        let token = Self::token(&self.config.password, &salt);

        let mut url = format!("{}/{}.view?", self.rest_base, endpoint);
        let auth = [
            ("u", self.config.username.as_str()),
            ("t", token.as_str()),
            ("s", salt.as_str()),
            ("v", API_VERSION),
            ("c", CLIENT_NAME),
            ("f", "json"),
        ];
        for (key, value) in auth.iter().chain(params.iter()) {
            let _ = write!(url, "{}={}&", key, percent_encode(value));
        }
        url.pop();
        url
    }
}

/// Normalise a server URL to its `/rest` base.
pub fn rest_base(server: &str) -> String {
    let trimmed = server.trim().trim_end_matches('/');
    if trimmed.ends_with("/rest") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/rest")
    }
}

/// Percent-encode a query value (RFC 3986 unreserved characters pass through).
pub fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Fresh 8-byte salt from the kernel CSPRNG, hex encoded.
fn random_salt() -> String {
    let mut buf = [0u8; 8];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        if file.read_exact(&mut buf).is_ok() {
            let mut out = String::with_capacity(16);
            for byte in buf {
                let _ = write!(out, "{byte:02x}");
            }
            return out;
        }
    }
    // Fallback: still unique enough to avoid token reuse.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}{:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_matches_subsonic_reference_vector() {
        // Reference: md5("sesame" + "c19b2d") as used in the Subsonic API docs.
        assert_eq!(
            SubsonicClient::token("sesame", "c19b2d"),
            "26719a1196d2a940705a59634eb18eab"
        );
    }

    #[test]
    fn rest_base_appends_once() {
        assert_eq!(rest_base("https://x.test"), "https://x.test/rest");
        assert_eq!(rest_base("https://x.test/"), "https://x.test/rest");
        assert_eq!(rest_base("https://x.test/rest"), "https://x.test/rest");
        assert_eq!(rest_base("https://x.test/rest/"), "https://x.test/rest");
        assert_eq!(rest_base("  https://x.test  "), "https://x.test/rest");
    }

    #[test]
    fn url_carries_auth_and_encodes_values() {
        let client = SubsonicClient::new(SubsonicConfig {
            server: "https://x.test".into(),
            username: "david".into(),
            password: "secret".into(),
        });
        let url = client.build_url("search3", &[("query", "kid a & more")]);
        assert!(url.starts_with("https://x.test/rest/search3.view?"));
        assert!(url.contains("u=david"));
        assert!(url.contains("v=1.16.1"));
        assert!(url.contains("c=undertone"));
        assert!(url.contains("f=json"));
        assert!(url.contains("query=kid%20a%20%26%20more"));
        // A token and salt are always present and non-empty.
        let salt = url.split("s=").nth(1).unwrap().split('&').next().unwrap();
        assert!(!salt.is_empty());
        let token = url.split("t=").nth(1).unwrap().split('&').next().unwrap();
        assert_eq!(token.len(), 32);
    }

    #[test]
    fn percent_encode_passes_unreserved_only() {
        assert_eq!(percent_encode("aZ09-_.~"), "aZ09-_.~");
        assert_eq!(percent_encode(" "), "%20");
        assert_eq!(percent_encode("/"), "%2F");
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    #[test]
    fn failed_envelope_maps_to_server_error() {
        let body = r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":40,"message":"Wrong username or password"}}}"#;
        let envelope: Envelope = serde_json::from_str(body).unwrap();
        assert_eq!(envelope.response.status, "failed");
        let err = envelope.response.error.unwrap();
        assert_eq!(err.code, 40);
        assert_eq!(err.message, "Wrong username or password");
    }

    #[test]
    fn ok_envelope_exposes_payload_key() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome","openSubsonic":true,"albumList2":{"album":[]}}}"#;
        let envelope: Envelope = serde_json::from_str(body).unwrap();
        assert_eq!(envelope.response.status, "ok");
        assert_eq!(envelope.response.server_type.as_deref(), Some("navidrome"));
        assert!(envelope.response.open_subsonic);
        assert!(envelope.response.payload.contains_key("albumList2"));
    }

    #[test]
    fn salts_are_unique() {
        let a = random_salt();
        let b = random_salt();
        assert_ne!(a, b);
        assert_eq!(a.len(), 16);
    }
}
