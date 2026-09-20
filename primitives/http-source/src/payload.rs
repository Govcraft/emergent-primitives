//! The `http.request` payload, built as a pure function of the request parts.
//!
//! # Why `query` is its own field
//!
//! `path` carries the URI path and nothing else. The query string, when there
//! is one, goes in a sibling `query` field, raw and undecoded.
//!
//! The alternative is folding `?a=1` into `path`, and it breaks the field's
//! only real job. Topologies route on `path` by equality: `select(.path ==
//! "/inject")` in an `exec-handler` selector, or a match in a downstream
//! service. A `path` that sometimes carries a query and sometimes does not
//! matches for one caller and misses for the next, and the failure is silent.
//! Keeping the two apart means `path` is stable per endpoint and `query` is
//! there in one piece for whoever wants it.
//!
//! `query` is left exactly as it arrived rather than parsed into a map: percent
//! decoding, repeated keys and bare flags have no single right answer, and a
//! consumer that cares can decode it the way its own API expects.

use std::collections::HashMap;
use std::net::IpAddr;

use axum::http::{HeaderMap, Method, Uri};

/// Payload for `http.request` events.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HttpRequestPayload {
    /// The request method, uppercase (`POST`, `GET`, ...).
    pub method: String,
    /// The path the client requested, without the query string.
    ///
    /// This is what the client asked for, not what `--path` was configured
    /// with. The two differ whenever `--path` uses axum's capture syntax, for
    /// example `--path '/hook/{id}'` serving a request for `/hook/42`.
    pub path: String,
    /// The raw, undecoded query string, or `null` when the request had none.
    pub query: Option<String>,
    /// Request headers, lowercased names. Headers whose value is not valid
    /// UTF-8 are dropped.
    pub headers: HashMap<String, String>,
    /// The request body, parsed as JSON when it parses, otherwise the body as a
    /// JSON string.
    pub body: serde_json::Value,
    /// The caller's IP address, with no port.
    ///
    /// See [`crate::addr`] for which address this is and when
    /// `--trust-forwarded-for` changes it.
    pub remote_addr: String,
}

/// Converts headers into a map, dropping any whose value is not valid UTF-8.
///
/// A header value is arbitrary bytes; a JSON string is not. The alternative to
/// dropping is lossy replacement, which would publish a value that is not the
/// one sent.
#[must_use]
pub fn headers_to_map(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|text| (name.as_str().to_string(), text.to_string()))
        })
        .collect()
}

/// Parses a request body as JSON, falling back to the body as a JSON string.
#[must_use]
pub fn parse_body(body: &[u8]) -> serde_json::Value {
    serde_json::from_slice(body)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(body).into_owned()))
}

/// Extracts the query string, treating a bare `?` as no query at all.
#[must_use]
pub fn query_of(uri: &Uri) -> Option<String> {
    uri.query()
        .filter(|query| !query.is_empty())
        .map(str::to_string)
}

/// Builds the published payload from the parts of one request.
#[must_use]
pub fn build_payload(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &[u8],
    remote_addr: IpAddr,
) -> HttpRequestPayload {
    HttpRequestPayload {
        method: method.to_string(),
        path: uri.path().to_string(),
        query: query_of(uri),
        headers: headers_to_map(headers),
        body: parse_body(body),
        remote_addr: remote_addr.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use serde_json::json;

    fn uri(text: &str) -> Uri {
        text.parse().unwrap_or(Uri::from_static("/"))
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap_or(IpAddr::from([0, 0, 0, 0]))
    }

    #[test]
    fn path_and_query_split_table() {
        // (uri, expected path, expected query)
        let cases: &[(&str, &str, Option<&str>)] = &[
            ("/", "/", None),
            ("/inject", "/inject", None),
            ("/inject?a=1", "/inject", Some("a=1")),
            ("/inject?a=1&b=2", "/inject", Some("a=1&b=2")),
            ("/inject?", "/inject", None),
            ("/hook/42", "/hook/42", None),
            ("/a%20b", "/a%20b", None),
            (
                "/inject?q=hello%20world",
                "/inject",
                Some("q=hello%20world"),
            ),
            ("/deep/nested/path?x", "/deep/nested/path", Some("x")),
            ("http://example.com/absolute?z=9", "/absolute", Some("z=9")),
        ];

        for (raw, expected_path, expected_query) in cases {
            let parsed = uri(raw);
            assert_eq!(parsed.path(), *expected_path, "path of {raw}");
            assert_eq!(
                query_of(&parsed).as_deref(),
                *expected_query,
                "query of {raw}"
            );
        }
    }

    #[test]
    fn body_parsing_table() {
        let cases: &[(&str, serde_json::Value)] = &[
            (r#"{"a":1}"#, json!({"a": 1})),
            ("[1,2,3]", json!([1, 2, 3])),
            ("null", serde_json::Value::Null),
            ("42", json!(42)),
            ("plain text", json!("plain text")),
            ("", json!("")),
            ("{not json", json!("{not json")),
        ];

        for (raw, expected) in cases {
            assert_eq!(parse_body(raw.as_bytes()), *expected, "body {raw:?}");
        }
    }

    #[test]
    fn headers_drop_non_utf8_values() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        if let Ok(value) = HeaderValue::from_bytes(&[0xff, 0xfe]) {
            headers.insert("x-binary", value);
        }

        let map = headers_to_map(&headers);
        assert_eq!(
            map.get("content-type").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(map.get("x-binary"), None);
    }

    #[test]
    fn build_payload_reports_the_requested_path_not_the_route() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.7"));

        let payload = build_payload(
            &Method::POST,
            &uri("/hook/42?dry=true"),
            &headers,
            br#"{"ok":true}"#,
            ip("203.0.113.7"),
        );

        assert_eq!(payload.method, "POST");
        assert_eq!(payload.path, "/hook/42");
        assert_eq!(payload.query.as_deref(), Some("dry=true"));
        assert_eq!(payload.body, json!({"ok": true}));
        assert_eq!(payload.remote_addr, "203.0.113.7");
    }

    #[test]
    fn remote_addr_serializes_as_a_string_never_null() {
        let payload = build_payload(
            &Method::GET,
            &uri("/"),
            &HeaderMap::new(),
            b"",
            ip("198.51.100.9"),
        );

        let encoded = serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null);
        assert_eq!(encoded.get("remote_addr"), Some(&json!("198.51.100.9")));
        assert_eq!(encoded.get("query"), Some(&serde_json::Value::Null));
    }
}
