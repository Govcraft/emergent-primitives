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
//! One rule is ours, not the router's. A `?` or a `#` in the literal part of a
//! path builds a router without complaint, and the route then matches nothing:
//! the router is only ever shown the path of a request, which ends where the
//! query string starts, and a fragment never leaves the client. The source would
//! start, report healthy, and answer 404 to everything, so that is refused too.
//!
//! The same goes for a literal character that no request path carries as
//! itself. The server answers 400 to a raw space, control character, `<`, `>`
//! or backtick before the router is asked, and curl and browsers send anything
//! outside ASCII percent-encoded. The router compares the raw, still encoded
//! path, so `--path /hük` is never reached by a request for `/h%C3%BCk`. Those
//! characters are refused with the spelling to use instead. Registering the
//! encoded form quietly was the other option, and was not taken: the published
//! `path` is the encoded one, so a downstream `select(.path == ...)` has to be
//! written that way, and the config should show what it has to match. Escapes
//! are also compared byte for byte (`%C3%BC` is not `%c3%bc`), which a silent
//! rewrite would hide.
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
//! router reason really does make `Router::route` panic. It also sends requests
//! at a `?` route and a `#` route to show that they answer 404 to all of them.

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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// A `?` outside a capture name. The router accepts it and never matches it.
    ContainsQuery,
    /// A `#` outside a capture name. The router accepts it and never matches it.
    ContainsFragment,
    /// A literal character that reaches the router only percent-encoded, or
    /// not at all. The router accepts it and never matches it.
    UnencodedLiteral {
        /// The first such character.
        character: char,
        /// The path with every such character percent-encoded.
        encoded_path: String,
    },
}

impl RoutePathError {
    /// Whether the router itself would have refused the path, by panicking.
    ///
    /// False for the rules that exist because the router accepts a path it can
    /// never match.
    #[must_use]
    pub fn is_router_rule(&self) -> bool {
        !matches!(
            self,
            Self::ContainsQuery | Self::ContainsFragment | Self::UnencodedLiteral { .. }
        )
    }
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
            Self::ContainsQuery => write!(
                f,
                "a route path cannot contain '?', the query string is not part of the route and is published in the `query` field"
            ),
            Self::UnencodedLiteral {
                character,
                encoded_path,
            } => write!(
                f,
                "a route path cannot contain {character:?} unencoded, a request carries it percent-encoded and the route must be spelled the same way: {encoded_path:?}"
            ),
            Self::ContainsFragment => write!(
                f,
                "a route path cannot contain '#', a fragment is never sent to the server so the route would match nothing"
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

/// Checks that `path` is a route axum 0.8 will register and can match.
///
/// The router's checks run first and in the order the router runs them, so the
/// rule reported is the one the panic would have named. The check for a route
/// that could never match comes last.
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
    let route = unescape(path);
    let captures = validate_captures(&route)?;
    validate_literals(path, &spans_in_path(&route, &captures))
}

/// One byte of the route once `{{` and `}}` have collapsed to a single brace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RouteByte {
    byte: u8,
    /// True for a brace that was written doubled, so it is a literal.
    escaped: bool,
    /// Where the byte sits in the path as it was written.
    offset: usize,
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
            offset: index,
        });
        index += if doubled { 2 } else { 1 };
    }
    route
}

/// Walks every capture in `route`, applies the rules that span them, and
/// returns where the captures are.
fn validate_captures(route: &[RouteByte]) -> Result<Vec<Range<usize>>, RoutePathError> {
    let mut from = 0;
    let mut named = 0;
    let mut wildcard_end = None;
    let mut captures = Vec::new();

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
        captures.push(capture);
    }

    match wildcard_end {
        Some(end) if end != route.len() => Err(RoutePathError::WildcardNotLast),
        _ => Ok(captures),
    }
}

/// Turns capture spans over `route` into byte spans over the written path.
fn spans_in_path(route: &[RouteByte], captures: &[Range<usize>]) -> Vec<Range<usize>> {
    captures
        .iter()
        .filter_map(|capture| {
            let open = route.get(capture.start)?;
            let close = route.get(capture.end.checked_sub(1)?)?;
            Some(open.offset..close.offset + 1)
        })
        .collect()
}

/// The characters of `path` outside `captures`, with their byte offsets.
fn literal_chars<'a>(
    path: &'a str,
    captures: &'a [Range<usize>],
) -> impl Iterator<Item = (usize, char)> + 'a {
    path.char_indices()
        .filter(|(offset, _)| !captures.iter().any(|capture| capture.contains(offset)))
}

/// Whether a literal `character` never reaches the router as itself.
///
/// The server answers 400 to a raw control character, space, `<`, `>` or
/// backtick. Anything outside ASCII is sent percent-encoded by curl and by
/// browsers. The characters between those two groups (`"`, `|`, `^`, `[`, `]`,
/// `\`, braces) are outside what a URL path strictly allows, but curl sends
/// them as they are and the route matches, so they are left alone.
fn needs_encoding(character: char) -> bool {
    character.is_ascii_control()
        || matches!(character, ' ' | '<' | '>' | '`')
        || !character.is_ascii()
}

/// `path` with every literal character that [`needs_encoding`] percent-encoded,
/// as UTF-8 and with uppercase hex, which is how curl and browsers send it.
/// Capture names are left as written.
fn encode_literals(path: &str, captures: &[Range<usize>]) -> String {
    let mut encoded = String::with_capacity(path.len());
    let mut utf8 = [0; 4];
    for (offset, character) in path.char_indices() {
        let literal = !captures.iter().any(|capture| capture.contains(&offset));
        if literal && needs_encoding(character) {
            for byte in character.encode_utf8(&mut utf8).bytes() {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        } else {
            encoded.push(character);
        }
    }
    encoded
}

/// Refuses the literal text a request can never match: a `?`, a `#`, or a
/// character that [`needs_encoding`].
///
/// A request's path never holds any of them as written. Inside a capture name
/// they are only part of a name, and the route works, so `captures` (byte spans
/// over `path`) are skipped.
fn validate_literals(path: &str, captures: &[Range<usize>]) -> Result<(), RoutePathError> {
    for (_, character) in literal_chars(path, captures) {
        match character {
            '?' => return Err(RoutePathError::ContainsQuery),
            '#' => return Err(RoutePathError::ContainsFragment),
            _ if needs_encoding(character) => {
                return Err(RoutePathError::UnencodedLiteral {
                    character,
                    encoded_path: encode_literals(path, captures),
                });
            }
            _ => {}
        }
    }
    Ok(())
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
            "/hook/{a b}/x",
            // Written the way a request carries it, a non-ASCII literal is fine.
            "/h%C3%BCk",
            "/hook%20with%20space",
            // Outside what a URL path strictly allows, but curl sends these as
            // they are and the route matches.
            "/a\"b|c^d[e]",
            // `?` and `#` are only refused in literal text. In a capture name
            // they are part of the name and the route matches as usual.
            "/hook/{id?}",
            "/hook/{id#}",
            "/{*rest?}",
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
            // The router takes these and then matches nothing.
            ("/hook?x=1", E::ContainsQuery),
            ("/hook?", E::ContainsQuery),
            ("/?", E::ContainsQuery),
            ("/hook/{id}?verbose", E::CaptureNotAtSegmentEnd),
            ("/hook/{id}/?verbose", E::ContainsQuery),
            ("/a?b/{*rest}", E::ContainsQuery),
            ("/hook#frag", E::ContainsFragment),
            ("/#", E::ContainsFragment),
            ("/hook/{id}/#top", E::ContainsFragment),
            // Whichever comes first is the one named.
            ("/hook?x=1#frag", E::ContainsQuery),
            ("/hook#frag?x=1", E::ContainsFragment),
            // A router rule still wins over these.
            ("hook?x=1", E::MissingLeadingSlash),
            ("/hook?x={", E::UnclosedCapture),
        ];

        for (path, expected) in cases {
            assert_eq!(
                validate_route_path(path),
                Err(expected.clone()),
                "path {path:?}"
            );
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
    fn unencoded_literals_table() {
        // (path, first offending character, the spelling to use)
        let cases: &[(&str, char, &str)] = &[
            ("/hük", 'ü', "/h%C3%BCk"),
            ("/hook with space", ' ', "/hook%20with%20space"),
            ("/a b/ü", ' ', "/a%20b/%C3%BC"),
            ("/日本", '日', "/%E6%97%A5%E6%9C%AC"),
            ("/emoji/🚀", '🚀', "/emoji/%F0%9F%9A%80"),
            ("/tab\there", '\t', "/tab%09here"),
            ("/del\u{7f}x", '\u{7f}', "/del%7Fx"),
            ("/lt<x", '<', "/lt%3Cx"),
            ("/gt>x", '>', "/gt%3Ex"),
            ("/bt`x", '`', "/bt%60x"),
            // Capture names are not literals: they stay as written, and only
            // the text around them is encoded.
            ("/ü/{naïve}/ü", 'ü', "/%C3%BC/{naïve}/%C3%BC"),
            ("/a b/{*the rest}", ' ', "/a%20b/{*the rest}"),
            // Doubled braces are literals and need no encoding themselves.
            ("/{{ü}}", 'ü', "/{{%C3%BC}}"),
        ];

        for (path, character, encoded) in cases {
            assert_eq!(
                validate_route_path(path),
                Err(RoutePathError::UnencodedLiteral {
                    character: *character,
                    encoded_path: (*encoded).to_string(),
                }),
                "path {path:?}"
            );
            assert_eq!(
                validate_route_path(encoded),
                Ok(()),
                "the suggested spelling {encoded:?} must itself be accepted"
            );
        }
    }

    #[test]
    fn whichever_unmatchable_literal_comes_first_is_named() {
        assert_eq!(
            validate_route_path("/a?b ü"),
            Err(RoutePathError::ContainsQuery)
        );
        assert!(matches!(
            validate_route_path("/a b?c"),
            Err(RoutePathError::UnencodedLiteral { character: ' ', .. })
        ));
        // A router rule still wins.
        assert_eq!(
            validate_route_path("/ü/{"),
            Err(RoutePathError::UnclosedCapture)
        );
    }

    #[test]
    fn the_unencoded_rule_shows_the_spelling_to_use() {
        let text = validate_route_path("/hook with space")
            .err()
            .map(|rule| rule.to_string())
            .unwrap_or_default();
        assert!(text.contains("' '"), "{text}");
        assert!(text.contains("\"/hook%20with%20space\""), "{text}");
        assert!(!text.contains('\n'), "{text}");
    }

    #[test]
    fn the_query_rule_says_where_the_query_string_goes() {
        let text = RoutePathError::ContainsQuery.to_string();
        assert!(text.contains("`query` field"), "{text}");
        assert!(text.contains("not part of the route"), "{text}");
    }

    #[test]
    fn only_the_unmatchable_rules_are_not_router_rules() {
        assert!(!RoutePathError::ContainsQuery.is_router_rule());
        assert!(!RoutePathError::ContainsFragment.is_router_rule());
        assert!(
            !RoutePathError::UnencodedLiteral {
                character: 'ü',
                encoded_path: "/%C3%BC".to_string(),
            }
            .is_router_rule()
        );
        assert!(RoutePathError::MissingLeadingSlash.is_router_rule());
        assert!(RoutePathError::WildcardNotLast.is_router_rule());
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
            E::ContainsQuery,
            E::ContainsFragment,
            E::UnencodedLiteral {
                character: '\n',
                encoded_path: "/%0A".to_string(),
            },
        ];

        for rule in rules {
            let text = rule.to_string();
            assert!(!text.is_empty(), "{rule:?} has no text");
            assert!(!text.contains('\n'), "{rule:?} spans lines: {text}");
        }
    }
}
