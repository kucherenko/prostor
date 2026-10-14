//! Framing of the ingest WebSocket's binary messages (protocol v1).
//!
//! Each binary message is `u32 big-endian seq` followed by exactly one complete FLV tag
//! (header, body, `PreviousTagSize`). The browser produces the tag with the wasm
//! `FlvMuxer` and prepends the sequence number; the server parses it with
//! [`parse_frame`], which refuses truncated and trailing bytes. Sequence numbers start at
//! 1 and increase by 1 per message; the server's acks are cumulative.

use crate::flv::{self, Tag};

/// Bytes of the sequence number prefix.
pub const SEQ_LEN: usize = 4;

/// Appends one framed message (`seq` + `tag`) to `out`. `tag` must already be a complete
/// FLV tag; nothing is validated here (the muxer produced it).
pub fn encode_frame(seq: u32, tag: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(tag);
}

/// Parses one framed message into its sequence number and tag. The message must hold
/// exactly one tag: fewer bytes are [`flv::Error::Truncated`], more are
/// [`flv::Error::TrailingBytes`].
pub fn parse_frame(message: &[u8]) -> Result<(u32, Tag<'_>), flv::Error> {
    if message.len() < SEQ_LEN {
        return Err(flv::Error::Truncated);
    }
    let seq = u32::from_be_bytes([message[0], message[1], message[2], message[3]]);
    let tag = flv::parse_exact(&message[SEQ_LEN..])?;
    Ok((seq, tag))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flv::{TagKind, write_tag};

    fn audio_tag() -> Vec<u8> {
        let mut tag = Vec::new();
        write_tag(TagKind::Audio, 21, &[0xAF, 1, 0xDE, 0xAD], &mut tag).unwrap();
        tag
    }

    #[test]
    fn round_trips_a_frame() {
        let tag = audio_tag();
        let mut message = Vec::new();
        encode_frame(0xDEAD_BEEF, &tag, &mut message);
        assert_eq!(message.len(), SEQ_LEN + tag.len());
        assert_eq!(&message[..4], &[0xDE, 0xAD, 0xBE, 0xEF]);
        let (seq, parsed) = parse_frame(&message).unwrap();
        assert_eq!(seq, 0xDEAD_BEEF);
        assert_eq!(parsed.kind, TagKind::Audio);
        assert_eq!(parsed.timestamp_ms, 21);
        assert_eq!(parsed.body, &[0xAF, 1, 0xDE, 0xAD]);
    }

    #[test]
    fn rejects_truncated_and_trailing_bytes() {
        let tag = audio_tag();
        let mut message = Vec::new();
        encode_frame(1, &tag, &mut message);
        for cut in 0..message.len() {
            assert_eq!(
                parse_frame(&message[..cut]).unwrap_err(),
                flv::Error::Truncated,
                "cut at {cut}"
            );
        }
        let mut trailing = message.clone();
        trailing.push(0);
        assert_eq!(
            parse_frame(&trailing).unwrap_err(),
            flv::Error::TrailingBytes(1)
        );
        assert_eq!(parse_frame(&[]).unwrap_err(), flv::Error::Truncated);
    }

    #[test]
    fn rejects_a_corrupt_tag_without_panicking() {
        let mut message = vec![0, 0, 0, 1];
        message.extend_from_slice(&[0xFF; 32]);
        assert!(parse_frame(&message).is_err());
    }
}
