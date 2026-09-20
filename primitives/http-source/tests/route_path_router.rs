//! Does `route_path` agree with the router it guards?
//!
//! `validate_route_path` is a transcription of rules that live in axum and
//! matchit, and a transcription can drift. These tests put the same strings
//! through both and compare the verdicts, so an axum upgrade that changes what
//! `Router::route` panics on fails here instead of in an operator's terminal.
//!
//! One family of rules is deliberately stricter than the router: a `?` or `#`
//! in literal text. The router registers those and then never matches them, so
//! for them the comparison is the other way round (the router must accept), and
//! `a_query_or_fragment_route_answers_404_to_everything` sends the requests that
//! show why they are refused. The same holds for a literal character a request
//! only carries percent-encoded, with one more assertion: the spelling the
//! error suggests is a route the same request does reach.
//!
//! The panics are caught with `catch_unwind`. That works under the test
//! profile; the release profile aborts on panic, which is one more reason the
//! binary must never reach one.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::any;
use emergent_client::EmergentMessage;
use http_source::app::{AppState, MessagePublisher, PublishFuture, build_router};
use http_source::route_path::{MAX_CAPTURES, RoutePath, RoutePathError, validate_route_path};
use tower::ServiceExt;

/// Accepts every message and keeps none.
struct Discard;

impl MessagePublisher for Discard {
    fn publish(&self, _message: EmergentMessage) -> PublishFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}

fn state() -> Arc<AppState> {
    Arc::new(AppState {
        publisher: Arc::new(Discard),
        secret: None,
        publish_type: "http.request".to_string(),
        trust_forwarded_for: false,
    })
}

/// Whether `Router::route` registers `path` without panicking.
fn router_accepts(path: &str) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        let _router: Router = Router::new().route(path, any(|| async {}));
    }))
    .is_ok()
}

/// A path with `count` named captures, `/{p0}/{p1}/...`.
fn path_with_captures(count: usize) -> String {
    (0..count).map(|index| format!("/{{p{index}}}")).collect()
}

/// Every shape the validator has an opinion on, valid and not.
fn corpus() -> Vec<String> {
    let fixed = [
        "",
        "bad-no-slash",
        "webhook/",
        "{id}",
        "/",
        "/webhook",
        "/webhook/",
        "/a/b/c",
        "//",
        "/a//b",
        "/hook/{id}",
        "/hook/{id}/",
        "/hook/{id}/events",
        "/hook/{*rest}",
        "/{*rest}",
        "/x{*rest}",
        "/{a}/{b}",
        "/v-{id}",
        "/{id}/{id}",
        "/{id}/x/{id}",
        "/{id}/{*id}",
        "/{a}/{*a}",
        "/hook/:id",
        "/:id",
        "/hook/*rest",
        "/*",
        "/hook/x:id",
        "/hook/x*rest",
        "/hook/{",
        "/hook/}",
        "/hook/{id",
        "/hook/id}",
        "/hook/{}",
        "/hook/{*}",
        "/hook/{*}x",
        "/hook/{}}x}",
        "/hook/{{id}}",
        "/hook/{{",
        "/hook/}}",
        "/hook/{{}",
        "/hook/{{a}",
        "/hook/{{*rest}}",
        "/hook/{a}}",
        "/hook/{a}}}",
        "/hook/{a{b}",
        "/hook/{a{b}}",
        "/hook/{a{{b}",
        "/hook/{a}}b}",
        "/hook/{a/b}",
        "/hook/{/}",
        "/hook/{/a}",
        "/hook/{a*b}",
        "/hook/{*a*}",
        "/hook/{**a}",
        "/hook/{a b}",
        "/hook/{ }",
        "/hook/{:id}",
        "/hook/{id:\\d+}",
        "/hook/{ü}",
        "/hük",
        "/hook/{a}{b}",
        "/hook/{a}-{b}",
        "/hook/{a}-x",
        "/hook/{a}.json",
        "/hook/{*rest}{a}",
        "/{a}{{",
        "/{a}}}x",
        "/hook/{*rest}/x",
        "/hook/{*rest}/",
        "/{*a}/{*b}",
        "/{*a}/{b}",
        "/hook/{*rest}/{",
        "/hook with space",
        "/%7Bid%7D",
        "/hook?x=1",
        "/hook?",
        "/?",
        "/hook#frag",
        "/#",
        "/hook?x=1#frag",
        "/hook/{id}/?verbose",
        "/hook/{id}?verbose",
        "/a?b/{*rest}",
        "/hook/{id?}",
        "/hook/{id#}",
        "/{*rest?}",
        "/hook?x={",
        "hook?x=1",
        "/h%C3%BCk",
        "/hook%20with%20space",
        "/a b/ü",
        "/日本",
        "/tab\there",
        "/del\u{7f}x",
        "/lt<x",
        "/gt>x",
        "/bt`x",
        "/ü/{naïve}/ü",
        "/{{ü}}",
        "/ü/{",
        "/a\"b|c^d[e]",
        "/hook/{a b}/x",
    ];

    let mut corpus: Vec<String> = fixed.iter().map(ToString::to_string).collect();
    corpus.push(path_with_captures(MAX_CAPTURES));
    corpus.push(path_with_captures(MAX_CAPTURES + 1));
    corpus.push(format!("{}/{{*rest}}", path_with_captures(MAX_CAPTURES)));
    corpus.push(format!(
        "{}/{{*rest}}",
        path_with_captures(MAX_CAPTURES + 1)
    ));
    corpus
}

#[test]
fn the_validator_and_the_router_give_the_same_verdict() {
    let mut stricter = 0;
    for path in corpus() {
        let verdict = validate_route_path(&path);
        // A rule of our own means the router has no objection: had it one, the
        // validator would have named that rule first.
        let router_should_accept = match &verdict {
            Ok(()) => true,
            Err(rule) if rule.is_router_rule() => false,
            Err(_) => {
                stricter += 1;
                true
            }
        };
        assert_eq!(
            router_should_accept,
            router_accepts(&path),
            "validator said {verdict:?} for {path:?}"
        );
    }
    assert!(stricter >= 18, "only {stricter} rows hit a rule of our own");
}

/// The status a router serving only `route` gives a request for `uri`.
async fn status_of(route: &str, uri: &str) -> StatusCode {
    let router: Router = Router::new().route(route, any(|| async {}));
    let Ok(request) = Request::builder().uri(uri).body(Body::empty()) else {
        panic!("could not build a request for {uri}");
    };
    match router.oneshot(request).await {
        Ok(response) => response.status(),
        Err(error) => panic!("router returned an error for {uri}: {error:?}"),
    }
}

#[tokio::test]
async fn a_query_or_fragment_route_answers_404_to_everything() {
    // (route the validator refuses, request that might be hoped to reach it)
    let cases = [
        // The request path stops at `?`, so the router is shown `/hook`.
        ("/hook?x=1", "/hook?x=1"),
        ("/hook?x=1", "/hook"),
        // Percent-encoding the `?` keeps it in the path, still encoded.
        ("/hook?x=1", "/hook%3Fx=1"),
        ("/hook?", "/hook?"),
        ("/hook?", "/hook"),
        // A fragment is dropped before the router sees the request.
        ("/hook#frag", "/hook#frag"),
        ("/hook#frag", "/hook"),
        ("/hook#frag", "/hook%23frag"),
    ];

    for (route, uri) in cases {
        assert!(
            validate_route_path(route).is_err_and(|rule| !rule.is_router_rule()),
            "{route:?} should be refused by a rule of our own"
        );
        assert!(router_accepts(route), "the router registers {route:?}");
        assert_eq!(
            status_of(route, uri).await,
            StatusCode::NOT_FOUND,
            "route {route:?}, request {uri:?}"
        );
    }
}

#[tokio::test]
async fn the_routes_that_are_kept_do_match() {
    // (accepted route, request, expected status)
    let cases = [
        // What the `?` route was reaching for: the query string is not part of
        // the route, and any query reaches the plain path.
        ("/hook", "/hook?x=1", StatusCode::OK),
        ("/hook", "/hook", StatusCode::OK),
        // In a capture name `?` and `#` are just part of the name.
        ("/hook/{id?}", "/hook/42", StatusCode::OK),
        ("/hook/{id#}", "/hook/42", StatusCode::OK),
        ("/{*rest?}", "/a/b", StatusCode::OK),
    ];

    for (route, uri, expected) in cases {
        assert_eq!(validate_route_path(route), Ok(()), "route {route:?}");
        assert_eq!(
            status_of(route, uri).await,
            expected,
            "route {route:?}, request {uri:?}"
        );
    }
}

#[test]
fn an_accepted_path_never_panics_in_build_router() {
    let mut accepted = 0;
    for path in corpus() {
        let Ok(route) = RoutePath::parse(&path) else {
            continue;
        };
        accepted += 1;
        let built = catch_unwind(AssertUnwindSafe(|| {
            let _router = build_router(&route, state());
        }));
        assert!(built.is_ok(), "build_router panicked on accepted {path:?}");
    }
    assert!(accepted > 20, "only {accepted} corpus rows were accepted");
}

#[tokio::test]
async fn an_unencoded_literal_route_is_missed_and_its_suggested_spelling_is_hit() {
    // (route the validator refuses, the request a client sends for it)
    let cases = [
        ("/hük", "/h%C3%BCk"),
        ("/hook with space", "/hook%20with%20space"),
        ("/a b/ü", "/a%20b/%C3%BC"),
        ("/日本", "/%E6%97%A5%E6%9C%AC"),
        ("/lt<x", "/lt%3Cx"),
        ("/bt`x", "/bt%60x"),
        ("/ü/{naïve}/ü", "/%C3%BC/42/%C3%BC"),
    ];

    for (route, uri) in cases {
        let Err(RoutePathError::UnencodedLiteral { encoded_path, .. }) = validate_route_path(route)
        else {
            panic!("{route:?} should be refused as an unencoded literal");
        };
        assert!(router_accepts(route), "the router registers {route:?}");
        assert_eq!(
            status_of(route, uri).await,
            StatusCode::NOT_FOUND,
            "route {route:?} as written, request {uri:?}"
        );
        assert_eq!(
            status_of(&encoded_path, uri).await,
            StatusCode::OK,
            "route {encoded_path:?} as suggested, request {uri:?}"
        );
    }
}

#[test]
fn some_refused_literals_cannot_even_be_put_in_a_request() {
    // The HTTP types refuse these in a request target, which is the same
    // answer the server gives on the wire: 400, before any routing.
    for uri in ["/hook with space", "/tab\there", "/lt<x", "/gt>x", "/bt`x"] {
        assert!(
            Request::builder().uri(uri).body(Body::empty()).is_err(),
            "a request for {uri:?} should not be buildable"
        );
        assert!(
            matches!(
                validate_route_path(uri),
                Err(RoutePathError::UnencodedLiteral { .. })
            ),
            "{uri:?} should be refused as an unencoded literal"
        );
    }
}

#[tokio::test]
async fn escapes_are_matched_byte_for_byte() {
    // Why the encoded form is not registered on the operator's behalf: there
    // is more than one, and the router tells them apart.
    assert_eq!(status_of("/h%C3%BCk", "/h%C3%BCk").await, StatusCode::OK);
    assert_eq!(
        status_of("/h%C3%BCk", "/h%c3%bck").await,
        StatusCode::NOT_FOUND
    );
    // The characters left alone do match as written.
    assert_eq!(
        status_of("/a\"b|c^d[e]", "/a\"b|c^d[e]").await,
        StatusCode::OK
    );
}
