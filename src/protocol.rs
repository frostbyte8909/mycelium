use bytes::{Buf, Bytes, BytesMut};
use std::{error::Error, fmt, str};
pub const MAX_HEADER_LEN: usize = 8 * 1024;
pub const MAX_KEY_LEN: usize = 1024;
pub const MAX_PAYLOAD_LEN: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Get {
        key: Bytes,
    },
    Put {
        key: Bytes,
        timestamp: u64,
        value: Bytes,
    },
    Delete {
        key: Bytes,
        timestamp: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    InvalidProtocol,
    FrameTooLarge,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProtocol => write!(f, "invalid protocol frame"),
            Self::FrameTooLarge => write!(f, "frame exceeds maximum allowed size"),
        }
    }
}

impl Error for ParseError {}

enum Header<'a> {
    Get { key: &'a str },
    Del { key: &'a str, timestamp: u64 },
    Put { key: &'a str, timestamp: u64, payload_len: usize },
}

impl Frame {
    pub fn parse(src: &mut BytesMut) -> Result<Option<Frame>, ParseError> {
        let header_end = match find_header_end(src)? {
            Some(i) => i,
            None => return Ok(None),
        };

        let header = parse_header(&src[..header_end])?;
        let header_len = header_end + 2;

        match header {
            Header::Get { key } => {
                let frame = Frame::Get { key: copy_key(key) };
                src.advance(header_len);
                Ok(Some(frame))
            }

            Header::Del { key, timestamp } => {
                let frame = Frame::Delete { key: copy_key(key), timestamp };
                src.advance(header_len);
                Ok(Some(frame))
            }

            Header::Put { key, timestamp, payload_len } => {
                let payload_start = header_len;
                let payload_end = payload_start + payload_len;
                let frame_len = payload_end + 2;

                if src.len() < frame_len {
                    return Ok(None);
                }

                if &src[payload_end..frame_len] != b"\r\n" {
                    return Err(ParseError::InvalidProtocol);
                }

                let frame = Frame::Put {
                    key: copy_key(key),
                    timestamp,
                    value: Bytes::copy_from_slice(&src[payload_start..payload_end]),
                };

                src.advance(frame_len);
                Ok(Some(frame))
            }
        }
    }
}

pub fn parse_frames(
    src: &mut BytesMut,
    out: &mut Vec<Frame>,
    max_frames: usize,
) -> Result<usize, ParseError> {
    let mut parsed = 0;

    while parsed < max_frames {
        match Frame::parse(src)? {
            Some(frame) => {
                out.push(frame);
                parsed += 1;
            }
            None => break,
        }
    }

    Ok(parsed)
}

fn find_header_end(src: &[u8]) -> Result<Option<usize>, ParseError> {
    let limit = src.len().min(MAX_HEADER_LEN + 2);

    match memchr::memmem::find(&src[..limit], b"\r\n") {
        Some(i) => Ok(Some(i)),
        None if src.len() >= MAX_HEADER_LEN + 2 => Err(ParseError::FrameTooLarge),
        None => Ok(None),
    }
}

fn parse_header(raw: &[u8]) -> Result<Header<'_>, ParseError> {
    if raw.iter().any(|b| b.is_ascii_control()) {
        return Err(ParseError::InvalidProtocol);
    }

    let header = str::from_utf8(raw).map_err(|_| ParseError::InvalidProtocol)?;
    let mut parts = header.split(' ');

    match parts.next().unwrap_or("") {
        "GET" => {
            let key = parse_key(parts.next())?;
            expect_end(&mut parts)?;
            Ok(Header::Get { key })
        }

        "DEL" => {
            let key = parse_key(parts.next())?;
            let timestamp = parse_timestamp(parts.next())?;
            expect_end(&mut parts)?;
            Ok(Header::Del { key, timestamp })
        }

        "PUT" => {
            let key = parse_key(parts.next())?;
            let timestamp = parse_timestamp(parts.next())?;
            let payload_len = parse_payload_len(parts.next())?;
            expect_end(&mut parts)?;
            Ok(Header::Put { key, timestamp, payload_len })
        }

        _ => Err(ParseError::InvalidProtocol),
    }
}

fn expect_end<'a>(parts: &mut impl Iterator<Item = &'a str>) -> Result<(), ParseError> {
    match parts.next() {
        None => Ok(()),
        Some(_) => Err(ParseError::InvalidProtocol),
    }
}

fn parse_key(value: Option<&str>) -> Result<&str, ParseError> {
    let key = value.ok_or(ParseError::InvalidProtocol)?;

    if key.is_empty() {
        Err(ParseError::InvalidProtocol)
    } else if key.len() > MAX_KEY_LEN {
        Err(ParseError::FrameTooLarge)
    } else {
        Ok(key)
    }
}

fn copy_key(key: &str) -> Bytes {
    Bytes::copy_from_slice(key.as_bytes())
}

enum NumError {
    Invalid,
    Overflow,
}

fn parse_decimal(value: Option<&str>) -> Result<u64, NumError> {
    let s = value.ok_or(NumError::Invalid)?;
    let b = s.as_bytes();

    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return Err(NumError::Invalid);
    }
    if b.len() > 1 && b[0] == b'0' {
        return Err(NumError::Invalid);
    }

    s.parse::<u64>().map_err(|_| NumError::Overflow)
}

fn parse_timestamp(value: Option<&str>) -> Result<u64, ParseError> {
    parse_decimal(value).map_err(|_| ParseError::InvalidProtocol)
}

fn parse_payload_len(value: Option<&str>) -> Result<usize, ParseError> {
    match parse_decimal(value) {
        Ok(n) if n <= MAX_PAYLOAD_LEN as u64 => Ok(n as usize),
        Ok(_) | Err(NumError::Overflow) => Err(ParseError::FrameTooLarge),
        Err(NumError::Invalid) => Err(ParseError::InvalidProtocol),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn buf(s: &[u8]) -> BytesMut {
        BytesMut::from(s)
    }

    fn assert_rejects(input: &[u8], expected: ParseError) {
        let mut src = buf(input);
        let before = src.clone();
        assert_eq!(Frame::parse(&mut src), Err(expected), "input: {:?}", input);
        assert_eq!(src, before, "buffer must not be consumed on error");
    }

    #[test]
    fn parses_get() {
        let mut src = buf(b"GET hello\r\n");
        assert_eq!(
            Frame::parse(&mut src),
            Ok(Some(Frame::Get { key: Bytes::from("hello") }))
        );
        assert!(src.is_empty());
    }

    #[test]
    fn parses_delete() {
        let mut src = buf(b"DEL hello 12345\r\n");
        assert_eq!(
            Frame::parse(&mut src),
            Ok(Some(Frame::Delete { key: Bytes::from("hello"), timestamp: 12345 }))
        );
        assert!(src.is_empty());
    }

    #[test]
    fn parses_put() {
        let mut src = buf(b"PUT hello 12345 5\r\nhello\r\n");
        assert_eq!(
            Frame::parse(&mut src),
            Ok(Some(Frame::Put {
                key: Bytes::from("hello"),
                timestamp: 12345,
                value: Bytes::from("hello"),
            }))
        );
        assert!(src.is_empty());
    }

    #[test]
    fn parses_binary_payload() {
        let payload = [0x00, 0xFF, 0x0D, 0x0A, 0x80];
        let mut input = b"PUT key 1 5\r\n".to_vec();
        input.extend_from_slice(&payload);
        input.extend_from_slice(b"\r\n");

        let mut src = buf(&input);
        assert_eq!(
            Frame::parse(&mut src),
            Ok(Some(Frame::Put {
                key: Bytes::from("key"),
                timestamp: 1,
                value: Bytes::copy_from_slice(&payload),
            }))
        );
        assert!(src.is_empty());
    }

    #[test]
    fn parses_zero_length_payload() {
        let mut src = buf(b"PUT k 1 0\r\n\r\n");
        assert_eq!(
            Frame::parse(&mut src),
            Ok(Some(Frame::Put {
                key: Bytes::from("k"),
                timestamp: 1,
                value: Bytes::new(),
            }))
        );
        assert!(src.is_empty());
    }

    #[test]
    fn accepts_zero_timestamp() {
        let mut src = buf(b"DEL k 0\r\n");
        assert!(matches!(Frame::parse(&mut src), Ok(Some(Frame::Delete { timestamp: 0, .. }))));
    }

    #[test]
    fn empty_buffer_needs_more() {
        let mut src = BytesMut::new();
        assert_eq!(Frame::parse(&mut src), Ok(None));
    }

    #[test]
    fn incomplete_inputs_do_not_consume() {
        for input in [
            &b"GET hel"[..],
            b"GET hello\r",
            b"DEL key 12",
            b"PUT key 1 5",
            b"PUT key 1 5\r\n",
            b"PUT key 1 5\r\nhel",
            b"PUT key 1 5\r\nhello",
            b"PUT key 1 5\r\nhello\r",
        ] {
            let mut src = buf(input);
            let before = src.clone();
            assert_eq!(Frame::parse(&mut src), Ok(None), "input: {:?}", input);
            assert_eq!(src, before);
        }
    }

    #[test]
    fn header_split_between_cr_and_lf() {
        let mut src = buf(b"GET k\r");
        assert_eq!(Frame::parse(&mut src), Ok(None));
        src.extend_from_slice(b"\n");
        assert!(matches!(Frame::parse(&mut src), Ok(Some(Frame::Get { .. }))));
        assert!(src.is_empty());
    }

    #[test]
    fn parses_multiple_frames() {
        let mut src = buf(b"GET one\r\nDEL two 123\r\nGET three\r\n");
        let mut out = Vec::new();

        assert_eq!(parse_frames(&mut src, &mut out, usize::MAX), Ok(3));
        assert_eq!(out.len(), 3);
        assert!(src.is_empty());
    }

    #[test]
    fn preserves_incomplete_trailing_frame() {
        let mut src = buf(b"GET one\r\nPUT two 123 5\r\nhel");
        let mut out = Vec::new();

        assert_eq!(parse_frames(&mut src, &mut out, usize::MAX), Ok(1));
        assert_eq!(out.len(), 1);
        assert_eq!(&src[..], b"PUT two 123 5\r\nhel");
    }

    #[test]
    fn frames_before_an_error_are_not_lost() {
        let mut src = buf(b"GET a\r\nGET b\r\nBAD\r\nGET c\r\n");
        let mut out = Vec::new();

        assert_eq!(
            parse_frames(&mut src, &mut out, usize::MAX),
            Err(ParseError::InvalidProtocol)
        );
        assert_eq!(out.len(), 2);
        assert_eq!(&src[..], b"BAD\r\nGET c\r\n");
    }

    #[test]
    fn max_frames_caps_a_single_call() {
        let mut src = buf(b"GET a\r\nGET b\r\nGET c\r\n");
        let mut out = Vec::new();

        assert_eq!(parse_frames(&mut src, &mut out, 2), Ok(2));
        assert_eq!(out.len(), 2);
        assert_eq!(&src[..], b"GET c\r\n");

        assert_eq!(parse_frames(&mut src, &mut out, 2), Ok(1));
        assert_eq!(out.len(), 3);
        assert!(src.is_empty());
    }

    #[test]
    fn rejects_unknown_and_empty_command() {
        assert_rejects(b"UNKNOWN key\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"get key\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"\r\n", ParseError::InvalidProtocol);
    }

    #[test]
    fn rejects_missing_and_extra_arguments() {
        assert_rejects(b"GET\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"GET key extra\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"DEL key\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"DEL key 1 extra\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"PUT key 1\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"PUT key 1 3 extra\r\n", ParseError::InvalidProtocol);
    }

    #[test]
    fn rejects_invalid_payload_terminator() {
        assert_rejects(b"PUT key 1 3\r\nabcXX", ParseError::InvalidProtocol);
    }

    #[test]
    fn rejects_lenient_whitespace_and_control_characters() {
        assert_rejects(b"GET\tkey\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"GET\nkey\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"GET key\r\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"GET  key\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"GET key \r\n", ParseError::InvalidProtocol);
        assert_rejects(b" GET key\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"GET ke\x00y\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"GET ke\x7fy\r\n", ParseError::InvalidProtocol);
    }

    #[test]
    fn rejects_invalid_utf8_header() {
        assert_rejects(b"GET \xFF\xFE\r\n", ParseError::InvalidProtocol);
    }

    #[test]
    fn rejects_invalid_and_non_canonical_numbers() {
        assert_rejects(b"DEL key not_a_number\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"DEL key +5\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"DEL key -5\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"DEL key 007\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"DEL key 1.5\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"DEL key 18446744073709551616\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"PUT key 1 +3\r\nabc\r\n", ParseError::InvalidProtocol);
        assert_rejects(b"PUT key 1 03\r\nabc\r\n", ParseError::InvalidProtocol);
    }

    #[test]
    fn accepts_u64_max_timestamp() {
        let mut src = buf(b"DEL key 18446744073709551615\r\n");
        assert!(matches!(
            Frame::parse(&mut src),
            Ok(Some(Frame::Delete { timestamp: u64::MAX, .. }))
        ));
    }

    #[test]
    fn rejects_oversized_key() {
        let mut ok = b"GET ".to_vec();
        ok.extend(std::iter::repeat(b'a').take(MAX_KEY_LEN));
        ok.extend_from_slice(b"\r\n");
        assert!(matches!(Frame::parse(&mut buf(&ok)), Ok(Some(_))));

        let mut too_big = b"GET ".to_vec();
        too_big.extend(std::iter::repeat(b'a').take(MAX_KEY_LEN + 1));
        too_big.extend_from_slice(b"\r\n");
        assert_rejects(&too_big, ParseError::FrameTooLarge);
    }

    #[test]
    fn rejects_oversized_payload_before_buffering_it() {
        let header = format!("PUT k 1 {}\r\n", MAX_PAYLOAD_LEN + 1);
        assert_rejects(header.as_bytes(), ParseError::FrameTooLarge);

        assert_rejects(b"PUT k 1 99999999999999999999999\r\n", ParseError::FrameTooLarge);
    }

    #[test]
    fn accepts_payload_at_exact_limit_header() {
        let header = format!("PUT k 1 {}\r\n", MAX_PAYLOAD_LEN);
        let mut src = buf(header.as_bytes());
        assert_eq!(Frame::parse(&mut src), Ok(None));
    }

    #[test]
    fn header_limit_is_independent_of_segmentation() {
        let mut data = vec![b'a'; MAX_HEADER_LEN];

        data.push(b'\r');
        assert_eq!(find_header_end(&data), Ok(None));

        data.push(b'\n');
        assert_eq!(find_header_end(&data), Ok(Some(MAX_HEADER_LEN)));

        let long = vec![b'a'; MAX_HEADER_LEN + 2];
        assert_eq!(find_header_end(&long), Err(ParseError::FrameTooLarge));

        assert_eq!(find_header_end(b"GET k"), Ok(None));
    }

    fn drive(stream: &[u8], chunk_sizes: &[usize]) -> (Vec<Frame>, Option<ParseError>) {
        let mut src = BytesMut::new();
        let mut out = Vec::new();
        let (mut pos, mut i) = (0, 0);

        while pos < stream.len() {
            let n = chunk_sizes[i % chunk_sizes.len()]
                .max(1)
                .min(stream.len() - pos);
            i += 1;

            src.extend_from_slice(&stream[pos..pos + n]);
            pos += n;

            if let Err(e) = parse_frames(&mut src, &mut out, usize::MAX) {
                return (out, Some(e));
            }
        }

        (out, None)
    }

    #[test]
    fn byte_at_a_time_matches_whole_for_every_split_point() {
        let mut stream = Vec::new();
        stream.extend_from_slice(b"GET one\r\n");
        stream.extend_from_slice(b"PUT two 123 5\r\nh\r\n!\r\n");
        stream.extend_from_slice(b"DEL three 9\r\n");
        stream.extend_from_slice(b"PUT empty 1 0\r\n\r\n");

        let whole = drive(&stream, &[stream.len()]);
        assert_eq!(whole.0.len(), 4);
        assert_eq!(whole.1, None);

        assert_eq!(drive(&stream, &[1]), whole);

        for split in 1..stream.len() {
            assert_eq!(drive(&stream, &[split, stream.len()]), whole, "split at {split}");
        }
    }

    fn arb_command() -> impl Strategy<Value = Vec<u8>> {
        let key = "[a-z0-9]{1,16}";

        prop_oneof![
            key.prop_map(|k| format!("GET {k}\r\n").into_bytes()),
            (key, any::<u64>()).prop_map(|(k, t)| format!("DEL {k} {t}\r\n").into_bytes()),
            (key, any::<u64>(), proptest::collection::vec(any::<u8>(), 0..64)).prop_map(
                |(k, t, v)| {
                    let mut out = format!("PUT {k} {t} {}\r\n", v.len()).into_bytes();
                    out.extend_from_slice(&v);
                    out.extend_from_slice(b"\r\n");
                    out
                }
            ),
        ]
    }

    proptest! {
        #[test]
        fn chunking_does_not_change_result(
            commands in proptest::collection::vec(arb_command(), 0..8),
            tail in proptest::collection::vec(any::<u8>(), 0..32),
            chunk_sizes in proptest::collection::vec(1usize..16, 1..8),
        ) {
            let mut stream: Vec<u8> = commands.concat();
            stream.extend_from_slice(&tail);

            let whole = drive(&stream, &[stream.len().max(1)]);
            let chunked = drive(&stream, &chunk_sizes);

            prop_assert_eq!(whole, chunked);
        }

        #[test]
        fn valid_streams_yield_one_frame_per_command(
            commands in proptest::collection::vec(arb_command(), 0..8),
            chunk_sizes in proptest::collection::vec(1usize..16, 1..8),
        ) {
            let stream: Vec<u8> = commands.concat();
            let (frames, err) = drive(&stream, &chunk_sizes);

            prop_assert_eq!(err, None);
            prop_assert_eq!(frames.len(), commands.len());
        }

        #[test]
        fn never_panics_on_arbitrary_input(
            data in proptest::collection::vec(any::<u8>(), 0..512),
        ) {
            let mut src = BytesMut::from(&data[..]);
            let mut out = Vec::new();
            let _ = parse_frames(&mut src, &mut out, usize::MAX);
        }
    }
}
