//! Does `route_path` agree with the router it guards?
//!
//! `validate_route_path` is a transcription of rules that live in axum and
//! matchit, and a transcription can drift. These tests put the same strings
//! through both and compare the verdicts, so an axum upgrade that changes what
//! `Router::route` panics on fails here instead of in an operator's terminal.
//!
//! The panics are caught with `catch_unwind`. That works under the test
//! profile; the release profile aborts on panic, which is one more reason the
//! binary must never reach one.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use axum::Router;
use axum::routing::any;
use emergent_client::EmergentMessage;
use http_source::app::{AppState, MessagePublisher, PublishFuture, build_router};
use http_source::route_path::{MAX_CAPTURES, RoutePath, validate_route_path};

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
    for path in corpus() {
        assert_eq!(
            validate_route_path(&path).is_ok(),
            router_accepts(&path),
            "validator said {:?} for {path:?}",
            validate_route_path(&path)
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
