//! Is `--path` a route the router will accept?
//!
//! `axum::Router::route` panics on a path it cannot register. That is a fair
//! contract for a route written in source code, and the wrong one for a route
//! typed into a config file: the operator gets a backtrace and an exit status
//! of 101 for what is a typo. So the path is checked here first, and the
//! router only ever sees a [`RoutePath`], which cannot be built from a string
//! that would make it panic.
//!
//! The rules are not invented. They are the ones axum 0.8 and the matchit 0.8
//! router underneath it enforce, found by feeding both the table in the tests
//! below and keeping whatever they said:
//!
//! - axum wants a leading `/`, and refuses a segment that starts with `:` or
//!   `*` because that was the capture syntax before 0.8 and would now be
//!   matched literally without a word of warning.
//! - matchit reads `{name}` as a capture and `{*name}` as a wildcard, with `{{`
//!   and `}}` standing for literal braces. A capture needs a name, has to run to
//!   the end of its segment (`/v-{id}` is a route, `/{id}.json` is not), and a
//!   wildcard has to be the last thing in the path. It also names captures `a`
//!   to `z` internally and panics when it runs out.
//!
//! Two things that look like they should be rules are not. Repeating a capture
//! name (`/{id}/{id}`) is accepted by the router, and since this primitive never
//! extracts captures (it publishes the concrete path the client asked for)
//! there is nothing for the repeat to confuse. And a capture name may contain
//! nearly anything, spaces and colons included.
//!
//! The scan works on bytes, as matchit does. Every byte it compares against is
//! ASCII, so a multi-byte character can never be mistaken for a brace.
//!
//! `tests/route_path_router.rs` holds the two halves of the bargain together:
//! every path accepted here builds a router, and every path rejected here for a
//! router reason really does make `Router::route` panic.

use std::fmt;
use std::ops::Range;

/// The most named captures one route can hold.
///
/// matchit renames captures `a`, `b`, `c` and so on while inserting, and panics
/// with "Too many route parameters" when the next name would pass `z`. That
/// happens on the 26th capture. Wildcards are not renamed and do not count.
pub const MAX_CAPTURES: usize = 25;

/// Why a `--path` value is not a usable route.
///
/// `Display` states the rule that was broken, without the offending value, so
/// the caller can name the value once in its own words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutePathError {
    /// The path is the empty string.
    Empty,
    /// The path does not start with `/`.
    MissingLeadingSlash,
    /// A segment starts with `:`, the capture syntax before axum 0.8.
    LegacyCapture,
    /// A segment starts with `*`, the wildcard syntax before axum 0.8.
    LegacyWildcard,
    /// A `}` with no `{` before it.
    UnmatchedClosingBrace,
    /// A `{` whose `}` never arrives.
    UnclosedCapture,
    /// `{}` or `{*}`.
    EmptyCaptureName,
    /// A `/` or a `*` inside a capture name.
    InvalidCaptureName,
    /// Something other than `/` follows a capture's closing brace.
    CaptureNotAtSegmentEnd,
    /// A `{*wildcard}` that is not the end of the path.
    WildcardNotLast,
    /// More than [`MAX_CAPTURES`] named captures.
    TooManyCaptures,
}

impl fmt::Display for RoutePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "a route path cannot be empty, use \"/\" for the root"),
            Self::MissingLeadingSlash => write!(f, "a route path must start with '/'"),
            Self::LegacyCapture => write!(
                f,
                "a path segment cannot start with ':', write a capture as {{name}}"
            ),
            Self::LegacyWildcard => write!(
                f,
                "a path segment cannot start with '*', write a wildcard as {{*name}}"
            ),
            Self::UnmatchedClosingBrace => write!(
                f,
                "'}}' has no matching '{{', write '}}}}' for a literal brace"
            ),
            Self::UnclosedCapture => write!(
                f,
                "'{{' has no matching '}}', write '{{{{' for a literal brace"
            ),
            Self::EmptyCaptureName => {
                write!(f, "a capture needs a name, as in {{id}} or {{*rest}}")
            }
            Self::InvalidCaptureName => write!(
                f,
                "a capture name cannot contain '/' or '*', only a leading '*' marks a wildcard"
            ),
            Self::CaptureNotAtSegmentEnd => write!(
                f,
                "a capture must end its path segment, so only '/' may follow its '}}'"
            ),
            Self::WildcardNotLast => {
                write!(f, "a {{*wildcard}} capture must be the end of the path")
            }
            Self::TooManyCaptures => write!(
                f,
                "a route path can hold at most {MAX_CAPTURES} named captures"
            ),
        }
    }
}

impl std::error::Error for RoutePathError {}

/// A `--path` value that is known to be a route the router accepts.
///
/// The only way to get one is [`RoutePath::parse`], which is what lets
/// [`crate::app::build_router`] hand the string to axum without a panic in
/// reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePath(String);

impl RoutePath {
    /// Checks `path` and wraps it.
    ///
    /// # Errors
    ///
    /// Returns the first rule `path` breaks. See [`validate_route_path`].
    pub fn parse(path: &str) -> Result<Self, RoutePathError> {
        validate_route_path(path).map(|()| Self(path.to_string()))
    }

    /// The route, exactly as it was given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RoutePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Checks that `path` is a route axum 0.8 will register.
///
/// The checks run in the order the router runs them, so the rule reported is
/// the one the panic would have named.
///
/// # Errors
///
/// Returns the first rule `path` breaks.
pub fn validate_route_path(path: &str) -> Result<(), RoutePathError> {
    if path.is_empty() {
        return Err(RoutePathError::Empty);
    }
    if !path.starts_with('/') {
        return Err(RoutePathError::MissingLeadingSlash);
    }
    for segment in path.split('/') {
        if segment.starts_with(':') {
            return Err(RoutePathError::LegacyCapture);
        }
        if segment.starts_with('*') {
            return Err(RoutePathError::LegacyWildcard);
        }
    }
    validate_captures(&unescape(path))
}

/// One byte of the route once `{{` and `}}` have collapsed to a single brace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RouteByte {
    byte: u8,
    /// True for a brace that was written doubled, so it is a literal.
    escaped: bool,
}

/// Collapses `{{` and `}}`, left to right, remembering which braces those were.
///
/// Left to right matters: `}}}` is a literal brace followed by a closing one,
/// never the other way round.
fn unescape(path: &str) -> Vec<RouteByte> {
    let bytes = path.as_bytes();
    let mut route = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let doubled = (byte == b'{' || byte == b'}') && bytes.get(index + 1) == Some(&byte);
        route.push(RouteByte {
            byte,
            escaped: doubled,
        });
        index += if doubled { 2 } else { 1 };
    }
    route
}

/// Walks every capture in `route` and applies the rules that span them.
fn validate_captures(route: &[RouteByte]) -> Result<(), RoutePathError> {
    let mut from = 0;
    let mut named = 0;
    let mut wildcard_end = None;

    while let Some(capture) = find_capture(route, from)? {
        if is_wildcard(route, &capture) {
            wildcard_end.get_or_insert(capture.end);
        } else {
            named += 1;
            if named > MAX_CAPTURES {
                return Err(RoutePathError::TooManyCaptures);
            }
        }
        from = capture.end;
    }

    match wildcard_end {
        Some(end) if end != route.len() => Err(RoutePathError::WildcardNotLast),
        _ => Ok(()),
    }
}

/// Whether the capture spanning `capture` is a `{*wildcard}`.
fn is_wildcard(route: &[RouteByte], capture: &Range<usize>) -> bool {
    route
        .get(capture.start + 1)
        .is_some_and(|first| first.byte == b'*')
}

/// Finds the next capture at or after `from`, as the span from `{` to `}`.
///
/// This is matchit's `find_wildcard`, rule for rule, including the two places
/// where it looks at a byte without asking whether the brace was escaped: the
/// byte after `{` when checking for an empty name, and the byte after `}` when
/// checking that the capture ends its segment.
fn find_capture(route: &[RouteByte], from: usize) -> Result<Option<Range<usize>>, RoutePathError> {
    for (start, current) in route.iter().enumerate().skip(from) {
        if current.byte == b'}' && !current.escaped {
            return Err(RoutePathError::UnmatchedClosingBrace);
        }
        if current.byte != b'{' || current.escaped {
            continue;
        }
        if route.get(start + 1).is_some_and(|next| next.byte == b'}') {
            return Err(RoutePathError::EmptyCaptureName);
        }
        return close_capture(route, start).map(Some);
    }
    Ok(None)
}

/// Finds the `}` closing the capture opened at `start`.
///
/// The byte right after `{` is never inspected here: that is where the `*` of a
/// wildcard sits.
fn close_capture(route: &[RouteByte], start: usize) -> Result<Range<usize>, RoutePathError> {
    for (index, current) in route.iter().enumerate().skip(start + 2) {
        match current.byte {
            b'}' if current.escaped => {}
            b'}' => {
                if index == start + 2 && is_wildcard(route, &(start..index)) {
                    return Err(RoutePathError::EmptyCaptureName);
                }
                if route.get(index + 1).is_some_and(|next| next.byte != b'/') {
                    return Err(RoutePathError::CaptureNotAtSegmentEnd);
                }
                return Ok(start..index + 1);
            }
            b'*' | b'/' => return Err(RoutePathError::InvalidCaptureName),
            _ => {}
        }
    }
    Err(RoutePathError::UnclosedCapture)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path with `count` named captures, `/{p0}/{p1}/...`.
    fn path_with_captures(count: usize) -> String {
        (0..count).map(|index| format!("/{{p{index}}}")).collect()
    }

    #[test]
    fn accepted_paths_table() {
        let cases: &[&str] = &[
            "/",
            "/webhook",
            "/webhook/",
            "/a/b/c",
            "//",
            "/hook/{id}",
            "/hook/{id}/",
            "/hook/{id}/events",
            "/hook/{*rest}",
            "/{*rest}",
            "/{a}/{b}",
            "/v-{id}",
            "/x{*rest}",
            // The router accepts a repeated name, and nothing here extracts
            // captures, so there is nothing for it to break.
            "/{id}/{id}",
            "/{id}/{*id}",
            // `:` and `*` are only refused at the start of a segment.
            "/hook/x:id",
            "/hook/x*rest",
            // Doubled braces are literals.
            "/hook/{{id}}",
            "/hook/{{",
            "/hook/}}",
            "/hook/{{*rest}}",
            // A capture name may hold nearly anything, a literal brace included.
            "/hook/{a b}",
            "/hook/{:id}",
            "/hook/{a{b}",
            "/hook/{a}}}",
            "/hook/{a}}b}",
            "/hook/{ü}",
            "/hük",
        ];

        for path in cases {
            assert_eq!(validate_route_path(path), Ok(()), "path {path:?}");
        }
    }

    #[test]
    fn rejected_paths_table() {
        use RoutePathError as E;

        let cases: &[(&str, RoutePathError)] = &[
            ("", E::Empty),
            ("bad-no-slash", E::MissingLeadingSlash),
            ("webhook/", E::MissingLeadingSlash),
            ("{id}", E::MissingLeadingSlash),
            // The capture syntax before axum 0.8.
            ("/hook/:id", E::LegacyCapture),
            ("/:id", E::LegacyCapture),
            ("/hook/*rest", E::LegacyWildcard),
            ("/*", E::LegacyWildcard),
            // Unbalanced braces.
            ("/hook/}", E::UnmatchedClosingBrace),
            ("/hook/id}", E::UnmatchedClosingBrace),
            ("/hook/{{}", E::UnmatchedClosingBrace),
            ("/hook/{", E::UnclosedCapture),
            ("/hook/{id", E::UnclosedCapture),
            ("/hook/{{a}", E::UnmatchedClosingBrace),
            ("/hook/{a{b}}", E::UnclosedCapture),
            // `}}` is a literal, so this capture never closes.
            ("/hook/{a}}", E::UnclosedCapture),
            // Empty names.
            ("/hook/{}", E::EmptyCaptureName),
            ("/hook/{*}", E::EmptyCaptureName),
            ("/hook/{*}x", E::EmptyCaptureName),
            ("/hook/{}}x}", E::EmptyCaptureName),
            // Names that cross a segment or carry a stray `*`.
            ("/hook/{a/b}", E::InvalidCaptureName),
            ("/hook/{a*b}", E::InvalidCaptureName),
            ("/hook/{*a*}", E::InvalidCaptureName),
            ("/hook/{**a}", E::InvalidCaptureName),
            // A capture has to run to the end of its segment.
            ("/hook/{a}{b}", E::CaptureNotAtSegmentEnd),
            ("/hook/{a}-{b}", E::CaptureNotAtSegmentEnd),
            ("/hook/{a}-x", E::CaptureNotAtSegmentEnd),
            ("/hook/{a}.json", E::CaptureNotAtSegmentEnd),
            ("/hook/{*rest}{a}", E::CaptureNotAtSegmentEnd),
            ("/{a}{{", E::CaptureNotAtSegmentEnd),
            // A wildcard has to be last.
            ("/hook/{*rest}/x", E::WildcardNotLast),
            ("/hook/{*rest}/", E::WildcardNotLast),
            ("/{*a}/{*b}", E::WildcardNotLast),
        ];

        for (path, expected) in cases {
            assert_eq!(validate_route_path(path), Err(*expected), "path {path:?}");
        }
    }

    #[test]
    fn capture_count_is_bounded_where_the_router_runs_out_of_names() {
        assert_eq!(
            validate_route_path(&path_with_captures(MAX_CAPTURES)),
            Ok(())
        );
        assert_eq!(
            validate_route_path(&path_with_captures(MAX_CAPTURES + 1)),
            Err(RoutePathError::TooManyCaptures)
        );
        // A wildcard is not renamed, so it does not use up a name.
        let with_wildcard = format!("{}/{{*rest}}", path_with_captures(MAX_CAPTURES));
        assert_eq!(validate_route_path(&with_wildcard), Ok(()));
    }

    #[test]
    fn parse_keeps_the_path_as_given() {
        let parsed = RoutePath::parse("/hook/{id}");
        assert_eq!(
            parsed.as_ref().map(RoutePath::as_str),
            Ok("/hook/{id}"),
            "as_str"
        );
        assert_eq!(
            parsed.map(|path| path.to_string()),
            Ok("/hook/{id}".to_string()),
            "display"
        );
        assert_eq!(
            RoutePath::parse("hook"),
            Err(RoutePathError::MissingLeadingSlash)
        );
    }

    #[test]
    fn every_rule_reads_as_one_line() {
        use RoutePathError as E;

        let rules = [
            E::Empty,
            E::MissingLeadingSlash,
            E::LegacyCapture,
            E::LegacyWildcard,
            E::UnmatchedClosingBrace,
            E::UnclosedCapture,
            E::EmptyCaptureName,
            E::InvalidCaptureName,
            E::CaptureNotAtSegmentEnd,
            E::WildcardNotLast,
            E::TooManyCaptures,
        ];

        for rule in rules {
            let text = rule.to_string();
            assert!(!text.is_empty(), "{rule:?} has no text");
            assert!(!text.contains('\n'), "{rule:?} spans lines: {text}");
        }
    }
}
