//! How ccnm talks to the copy of itself on the other machine.
//!
//! Requests ride on an ssh command line, which the remote login shell
//! parses. Rather than quoting JSON for an unknown shell, the request is
//! serialized to JSON and base64url-encoded, so the only characters on the
//! wire are `[A-Za-z0-9_-]` (design doc section 16). Responses come back on
//! stdout, which no shell touches, so they stay plain JSON.
//!
//! Every message carries `protocol`; a mismatch means the two ccnm builds
//! disagree and is reported as `CCNM_E_VERSION` before anything is trusted.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, ErrorCode, Result};

/// Bump when a request or response shape changes incompatibly.
pub const PROTOCOL: u32 = 1;

/// Implemented by every message so the decoder can check its version.
pub trait Protocol {
    fn protocol(&self) -> u32;
}

/// JSON -> base64url, for argv.
pub fn encode<T: Serialize>(value: &T) -> Result<String> {
    let json = serde_json::to_vec(value)
        .map_err(|e| Error::internal("cannot encode payload").with_source(e))?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

/// One-line JSON for a stdout reply.
pub fn to_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| Error::internal("cannot encode reply").with_source(e))
}

/// base64url -> JSON -> `T`, checking `protocol`.
pub fn decode<T: DeserializeOwned + Protocol>(text: &str) -> Result<T> {
    let bytes = URL_SAFE_NO_PAD.decode(text.trim()).map_err(|e| {
        Error::new(
            ErrorCode::Version,
            "payload is not base64url; ccnm versions probably differ",
        )
        .with_source(e)
    })?;
    decode_json(&bytes)
}

/// Plain JSON -> `T`, checking `protocol`. For stdout responses.
pub fn decode_json<T: DeserializeOwned + Protocol>(bytes: &[u8]) -> Result<T> {
    let value: T = serde_json::from_slice(bytes).map_err(|e| {
        Error::new(
            ErrorCode::Version,
            format!(
                "message is not valid for protocol {PROTOCOL}; ccnm versions probably differ\nreceived: {}",
                preview(bytes)
            ),
        )
        .with_source(e)
    })?;
    if value.protocol() != PROTOCOL {
        return Err(Error::new(
            ErrorCode::Version,
            format!(
                "remote ccnm speaks protocol {}, this one speaks {PROTOCOL}",
                value.protocol()
            ),
        ));
    }
    Ok(value)
}

fn preview(bytes: &[u8]) -> String {
    const MAX: usize = 200;
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    if text.is_empty() {
        "<empty>".to_string()
    } else if text.len() > MAX {
        let end = text
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= MAX)
            .last()
            .unwrap_or(0);
        format!("{}...", &text[..end])
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Msg {
        protocol: u32,
        text: String,
    }

    impl Protocol for Msg {
        fn protocol(&self) -> u32 {
            self.protocol
        }
    }

    #[test]
    fn roundtrip_and_wire_charset() {
        let msg = Msg {
            protocol: PROTOCOL,
            text: "cargo test && echo '$HOME' | grep \"x\"\n".into(),
        };
        let wire = encode(&msg).unwrap();
        assert!(
            wire.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{wire}"
        );
        assert_eq!(decode::<Msg>(&wire).unwrap(), msg);
    }

    #[test]
    fn bad_base64_is_a_version_error() {
        let err = decode::<Msg>("not*base64").unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
    }

    #[test]
    fn wrong_shape_is_a_version_error_with_preview() {
        let err = decode_json::<Msg>(b"{\"unrelated\": 1}").unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message().contains("{\"unrelated\": 1}"), "{err}");
    }

    #[test]
    fn wrong_protocol_is_rejected() {
        let wire = encode(&Msg {
            protocol: PROTOCOL + 1,
            text: String::new(),
        })
        .unwrap();
        let err = decode::<Msg>(&wire).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message().contains("protocol 2"), "{err}");
    }

    #[test]
    fn empty_response_says_so() {
        let err = decode_json::<Msg>(b"  ").unwrap_err();
        assert!(err.message().contains("<empty>"), "{err}");
    }
}
