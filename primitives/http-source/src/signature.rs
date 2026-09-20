//! HMAC-SHA256 webhook signature validation.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Validates an HMAC-SHA256 signature over a request body.
///
/// The signature is hex, with an optional `sha256=` prefix, which is the shape
/// GitHub and most webhook senders emit. Verification is constant time, via
/// `Mac::verify_slice`.
#[must_use]
pub fn validate_signature(secret: &str, body: &[u8], signature: &str) -> bool {
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };

    mac.update(body);

    let Ok(expected) = hex::decode(signature.trim_start_matches("sha256=")) else {
        return false;
    };

    mac.verify_slice(&expected).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HMAC-SHA256 of `{"event":"test"}` keyed with `my-secret`.
    const BODY: &[u8] = br#"{"event":"test"}"#;
    const SECRET: &str = "my-secret";

    fn signature_for(secret: &str, body: &[u8]) -> String {
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
            return String::new();
        };
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn signature_table() {
        let valid = signature_for(SECRET, BODY);
        let prefixed = format!("sha256={valid}");

        // (case, signature, expected)
        let cases: &[(&str, &str, bool)] = &[
            ("bare hex", valid.as_str(), true),
            ("sha256= prefixed", prefixed.as_str(), true),
            ("wrong signature", &"00".repeat(32), false),
            ("not hex", "zzzz", false),
            ("empty", "", false),
            ("truncated", &valid[..8], false),
        ];

        for (case, signature, expected) in cases {
            assert_eq!(
                validate_signature(SECRET, BODY, signature),
                *expected,
                "case: {case}"
            );
        }
    }

    #[test]
    fn a_different_secret_or_body_does_not_validate() {
        let valid = signature_for(SECRET, BODY);
        assert!(!validate_signature("other-secret", BODY, &valid));
        assert!(!validate_signature(SECRET, b"tampered", &valid));
    }
}
