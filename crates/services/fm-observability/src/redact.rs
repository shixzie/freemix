use std::net::{Ipv4Addr, Ipv6Addr};

/// Stateless, deterministic redaction for operational text.
#[derive(Clone, Copy, Debug, Default)]
pub struct Redactor;

impl Redactor {
    pub const SECRET_MARKER: &'static str = "[REDACTED_SECRET]";
    pub const PATH_MARKER: &'static str = "[REDACTED_PATH]";
    pub const IP_MARKER: &'static str = "[REDACTED_IP]";

    /// Redacts common inline credentials, absolute paths, and IP addresses.
    #[must_use]
    pub fn redact(self, value: &str) -> String {
        let value = redact_secrets(value);
        let value = redact_paths(&value);
        redact_ips(&value)
    }

    /// Redacts the entire value when its structured field name is sensitive.
    #[must_use]
    pub fn redact_field(self, name: &str, value: &str) -> String {
        if self.is_secret_name(name) {
            Self::SECRET_MARKER.to_owned()
        } else {
            self.redact(value)
        }
    }

    #[must_use]
    pub fn is_secret_name(self, name: &str) -> bool {
        let normalized: String = name
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|character| character.to_ascii_lowercase())
            .collect();
        [
            "authorization",
            "password",
            "passwd",
            "token",
            "apikey",
            "secret",
            "streamkey",
            "credential",
            "cookie",
            "privatekey",
            "passphrase",
        ]
        .iter()
        .any(|secret| normalized.contains(secret))
    }
}

fn redact_secrets(value: &str) -> String {
    const NAMES: [&str; 18] = [
        "authorization",
        "password",
        "passwd",
        "api_key",
        "api-key",
        "apikey",
        "stream_key",
        "stream-key",
        "streamkey",
        "private_key",
        "private-key",
        "privatekey",
        "credential",
        "cookie",
        "token",
        "secret",
        "passphrase",
        // SRT carries the stream key as its `streamid` query value.
        "streamid",
    ];

    let lower = value.to_ascii_lowercase();
    let bytes = value.as_bytes();
    let mut ranges = Vec::new();
    for name in NAMES {
        for (start, _) in lower.match_indices(name) {
            let name_end = start + name.len();
            if !is_name_boundary(bytes, start, name_end) {
                continue;
            }
            let mut separator = name_end;
            while bytes
                .get(separator)
                .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'"' || *byte == b'\'')
            {
                separator += 1;
            }
            if !matches!(bytes.get(separator), Some(b':' | b'=')) {
                continue;
            }
            let mut secret_start = separator + 1;
            while bytes.get(secret_start).is_some_and(u8::is_ascii_whitespace) {
                secret_start += 1;
            }
            let quote = bytes
                .get(secret_start)
                .copied()
                .filter(|byte| matches!(byte, b'"' | b'\''));
            if quote.is_some() {
                secret_start += 1;
            }
            let mut secret_end = secret_start;
            if name == "authorization" {
                secret_end += authorization_scheme_len(&bytes[secret_start..]);
            }
            while let Some(byte) = bytes.get(secret_end) {
                let end = if let Some(quote) = quote {
                    *byte == quote
                } else if name == "cookie" {
                    // A Cookie header joins every cookie with `; `, and each
                    // one may be a session credential.
                    matches!(byte, b',' | b'\n' | b'\r')
                } else {
                    byte.is_ascii_whitespace() || matches!(byte, b'&' | b',' | b';' | b'"' | b'\'')
                };
                if end {
                    break;
                }
                secret_end += 1;
            }
            if secret_start < secret_end {
                ranges.push((secret_start, secret_end, Redactor::SECRET_MARKER));
            }
        }
    }

    push_bearer_ranges(&lower, bytes, &mut ranges);

    // URL user-info (`scheme://user:password@host`) is always credential data.
    for (scheme, _) in value.match_indices("://") {
        let authority_start = scheme + 3;
        let authority_end = bytes[authority_start..]
            .iter()
            .position(|byte| matches!(byte, b'/' | b'?' | b'#') || byte.is_ascii_whitespace())
            .map_or(bytes.len(), |offset| authority_start + offset);
        if let Some(at) = value[authority_start..authority_end].rfind('@') {
            ranges.push((
                authority_start,
                authority_start + at,
                Redactor::SECRET_MARKER,
            ));
        }
    }

    push_rtmp_stream_key_ranges(value, &mut ranges);

    replace_ranges(value, ranges)
}

/// RTMP-family URLs carry the stream key as the final path segment
/// (`rtmp://host/app/KEY`). When the path has at least two segments the last
/// one is redacted; the host and application name are kept. Other schemes
/// (HTTP, HLS playlists) are untouched.
fn push_rtmp_stream_key_ranges(value: &str, ranges: &mut Vec<(usize, usize, &str)>) {
    const SCHEMES: [&str; 6] = ["rtmp", "rtmps", "rtmpt", "rtmpe", "rtmpte", "rtmpts"];
    let bytes = value.as_bytes();
    for (separator, _) in value.match_indices("://") {
        let scheme_start = bytes[..separator]
            .iter()
            .rposition(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'+' | b'-' | b'.'))
            .map_or(0, |position| position + 1);
        let scheme = &value[scheme_start..separator];
        if !SCHEMES
            .iter()
            .any(|candidate| scheme.eq_ignore_ascii_case(candidate))
        {
            continue;
        }
        let authority_start = separator + 3;
        let url_end = bytes[authority_start..]
            .iter()
            .position(|byte| {
                byte.is_ascii_whitespace()
                    || matches!(byte, b'?' | b'#' | b'"' | b'\'' | b'<' | b'>')
            })
            .map_or(bytes.len(), |offset| authority_start + offset);
        let Some(path_offset) = value[authority_start..url_end].find('/') else {
            continue;
        };
        let path_start = authority_start + path_offset;
        let path = value[path_start..url_end].trim_end_matches('/');
        let segments = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .count();
        if segments < 2 {
            continue;
        }
        let Some(last_slash) = path.rfind('/') else {
            continue;
        };
        let key_start = path_start + last_slash + 1;
        let key_end = path_start + path.len();
        if key_start < key_end {
            ranges.push((key_start, key_end, Redactor::SECRET_MARKER));
        }
    }
}

/// Covers the token after every standalone `Bearer` scheme word.
fn push_bearer_ranges(lower: &str, bytes: &[u8], ranges: &mut Vec<(usize, usize, &str)>) {
    for (start, _) in lower.match_indices("bearer ") {
        if start > 0 && bytes[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let mut secret_start = start + "bearer ".len();
        // Padding between the scheme and the token is not the token's end.
        while bytes
            .get(secret_start)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        {
            secret_start += 1;
        }
        let secret_end = bytes[secret_start..]
            .iter()
            .position(|byte| byte.is_ascii_whitespace() || matches!(byte, b',' | b';' | b'"'))
            .map_or(bytes.len(), |offset| secret_start + offset);
        if secret_start < secret_end {
            ranges.push((secret_start, secret_end, Redactor::SECRET_MARKER));
        }
    }
}

/// Length of a leading `Bearer `/`Basic `/`Digest ` style scheme word, so the
/// credential that follows it is covered by the same redaction range.
fn authorization_scheme_len(bytes: &[u8]) -> usize {
    let scheme = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_alphabetic())
        .count();
    let padding = bytes[scheme..]
        .iter()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    if scheme > 0 && padding > 0 {
        scheme + padding
    } else {
        0
    }
}

fn is_name_boundary(bytes: &[u8], start: usize, end: usize) -> bool {
    // A leading `_` still names a secret (`access_token`, `client_secret`).
    let valid_start = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
    let valid_end = end == bytes.len() || !bytes[end].is_ascii_alphanumeric() && bytes[end] != b'_';
    valid_start && valid_end
}

fn redact_paths(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let windows = index + 2 < bytes.len()
            && bytes[index].is_ascii_alphabetic()
            && bytes[index + 1] == b':'
            && matches!(bytes[index + 2], b'/' | b'\\');
        let unc = bytes[index..].starts_with(b"\\\\");
        let posix = bytes[index] == b'/'
            && bytes
                .get(index.wrapping_sub(1))
                .is_none_or(|byte| *byte != b':')
            && bytes.get(index + 1).is_some_and(|byte| *byte != b'/');
        let boundary = index == 0
            || bytes[index - 1].is_ascii_whitespace()
            || matches!(bytes[index - 1], b'"' | b'\'' | b'=' | b'(' | b'[');

        if boundary && (windows || unc || posix) {
            let quote = index
                .checked_sub(1)
                .and_then(|previous| bytes.get(previous))
                .copied()
                .filter(|byte| matches!(byte, b'"' | b'\''));
            let mut end = index;
            while let Some(byte) = bytes.get(end) {
                let reached_end = quote.map_or_else(
                    || byte.is_ascii_whitespace() || matches!(byte, b',' | b';' | b')' | b']'),
                    |quote| *byte == quote,
                );
                if reached_end {
                    break;
                }
                end += 1;
            }
            ranges.push((index, end, Redactor::PATH_MARKER));
            index = end;
        } else {
            index += 1;
        }
    }
    replace_ranges(value, ranges)
}

fn redact_ips(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'['
            && let Some(close_offset) = value[index + 1..].find(']')
        {
            let end = index + 1 + close_offset;
            let candidate = &value[index + 1..end];
            if parse_ipv6(candidate) {
                ranges.push((index, end + 1, Redactor::IP_MARKER));
                index = end + 1;
                continue;
            }
        }

        if bytes[index].is_ascii_digit()
            && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric())
        {
            let mut end = index;
            while bytes
                .get(end)
                .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'.')
            {
                end += 1;
            }
            // Sentence punctuation after an address is not part of it.
            let candidate = value[index..end].trim_end_matches('.');
            if candidate.contains('.') && candidate.parse::<Ipv4Addr>().is_ok() {
                let end = index + candidate.len();
                ranges.push((index, end, Redactor::IP_MARKER));
                index = end;
                continue;
            }
        }

        let ipv6_start = bytes[index] == b':' || bytes[index].is_ascii_hexdigit();
        let boundary =
            index == 0 || !bytes[index - 1].is_ascii_alphanumeric() && bytes[index - 1] != b':';
        if ipv6_start && boundary {
            let mut end = index;
            while bytes.get(end).is_some_and(|byte| {
                byte.is_ascii_hexdigit()
                    || *byte == b':'
                    || *byte == b'%'
                    || (value[index..end].contains('%') && byte.is_ascii_alphanumeric())
            }) {
                end += 1;
            }
            let candidate = &value[index..end];
            if candidate.matches(':').count() >= 2 && parse_ipv6(candidate) {
                ranges.push((index, end, Redactor::IP_MARKER));
                index = end;
                continue;
            }
        }
        index += 1;
    }
    replace_ranges(value, ranges)
}

fn parse_ipv6(candidate: &str) -> bool {
    candidate
        .split_once('%')
        .map_or(candidate, |(address, _)| address)
        .parse::<Ipv6Addr>()
        .is_ok()
}

fn replace_ranges(value: &str, mut ranges: Vec<(usize, usize, &str)>) -> String {
    ranges.sort_unstable_by_key(|range| (range.0, range.1));
    let mut output = String::with_capacity(value.len());
    let mut copied = 0;
    for (start, end, marker) in ranges {
        if start < copied || start >= end || end > value.len() {
            continue;
        }
        output.push_str(&value[copied..start]);
        output.push_str(marker);
        copied = end;
    }
    output.push_str(&value[copied..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catches_secret_path_and_ip_traps() {
        let input = concat!(
            "password=hunter2 Authorization: Bearer abc.def ",
            "url=https://alice:p4ss@example.test/live?token=query-secret ",
            "posix=\"/Users/alice/My Show/key.mov\" win='C:\\Users\\alice\\show.mov' ",
            "peers=192.168.1.9:9000,[2001:db8::1]:443,::1"
        );
        let redacted = Redactor.redact(input);

        for leaked in [
            "hunter2",
            "abc.def",
            "alice:p4ss",
            "query-secret",
            "/Users/alice",
            "C:\\Users",
            "192.168.1.9",
            "2001:db8::1",
            "::1",
        ] {
            assert!(!redacted.contains(leaked), "leaked {leaked}: {redacted}");
        }
        assert!(redacted.contains(Redactor::SECRET_MARKER));
        assert!(redacted.contains(Redactor::PATH_MARKER));
        assert!(redacted.contains(Redactor::IP_MARKER));
    }

    #[test]
    fn underscore_prefixed_secret_names_are_redacted() {
        let redacted = Redactor
            .redact("url?access_token=tok-leak&x=1 client_secret=sec-leak db_password: pw-leak");
        for leaked in ["tok-leak", "sec-leak", "pw-leak"] {
            assert!(!redacted.contains(leaked), "leaked {leaked}: {redacted}");
        }
        assert!(redacted.contains("x=1"));
        assert_eq!(Redactor.redact("max_tokens=5"), "max_tokens=5");
    }

    #[test]
    fn compact_api_and_stream_key_names_are_redacted() {
        let redacted = Redactor
            .redact("apiKey=api-leak stream-key=live-leak streamKey: sk-leak private_key=pk-leak");
        for leaked in ["api-leak", "live-leak", "sk-leak", "pk-leak"] {
            assert!(!redacted.contains(leaked), "leaked {leaked}: {redacted}");
        }
    }

    #[test]
    fn non_bearer_authorization_credentials_are_redacted() {
        let redacted = Redactor.redact("Authorization: Basic dXNlcjpodW50ZXIy next");
        assert!(!redacted.contains("dXNlcjpodW50ZXIy"), "{redacted}");
        assert!(redacted.ends_with(" next"), "{redacted}");
    }

    #[test]
    fn credentials_after_padded_authorization_schemes_are_redacted() {
        for input in [
            "Authorization: Bearer  pad-leak next",
            "authorization=Basic\tpad-leak next",
            "bearer   pad-leak next",
        ] {
            let redacted = Redactor.redact(input);
            assert!(!redacted.contains("pad-leak"), "{redacted}");
            assert!(redacted.ends_with(" next"), "{redacted}");
        }
    }

    #[test]
    fn url_userinfo_with_unencoded_at_sign_is_fully_redacted() {
        let redacted = Redactor.redact("rtmp://user:p@ss@ingest.example/app");
        assert!(!redacted.contains("ss@"), "{redacted}");
        assert!(redacted.contains("ingest.example/app"), "{redacted}");
    }

    #[test]
    fn every_cookie_in_a_cookie_header_is_redacted() {
        let redacted = Redactor.redact("Cookie: theme=dark; session=sess-leak; id=id-leak\nnext");
        for leaked in ["sess-leak", "id-leak"] {
            assert!(!redacted.contains(leaked), "leaked {leaked}: {redacted}");
        }
        assert!(redacted.ends_with("\nnext"), "{redacted}");
    }

    #[test]
    fn srt_stream_ids_and_passphrases_are_redacted() {
        let redacted = Redactor.redact(
            "srt://ingest.example:9000?streamid=sid-leak&passphrase=pass-leak&latency=120 \
             SRT_PASSPHRASE=env-leak",
        );
        for leaked in ["sid-leak", "pass-leak", "env-leak"] {
            assert!(!redacted.contains(leaked), "leaked {leaked}: {redacted}");
        }
        assert!(redacted.contains("latency=120"), "{redacted}");
        assert!(Redactor.is_secret_name("srt.passphrase"));
        assert_eq!(
            Redactor.redact_field("passphrase", "12345"),
            Redactor::SECRET_MARKER
        );
    }

    #[test]
    fn rtmp_stream_keys_in_the_last_path_segment_are_redacted() {
        for (input, expected) in [
            (
                "push rtmp://live.example/app/sk-live-123 ok",
                "push rtmp://live.example/app/[REDACTED_SECRET] ok",
            ),
            (
                "RTMPS://a.rtmps.example:443/rtmp/FB-key-9?x=1",
                "RTMPS://a.rtmps.example:443/rtmp/[REDACTED_SECRET]?x=1",
            ),
            (
                "rtmp://live.example/app/inst/key-7/",
                "rtmp://live.example/app/inst/[REDACTED_SECRET]/",
            ),
            // Only an application name: nothing to redact.
            ("rtmp://live.example/app", "rtmp://live.example/app"),
        ] {
            assert_eq!(Redactor.redact(input), expected);
        }
        for unaffected in [
            "https://cdn.example/hls/stream/index.m3u8",
            "http://cdn.example/live/channel/playlist.m3u8",
        ] {
            assert_eq!(Redactor.redact(unaffected), unaffected);
        }
    }

    #[test]
    fn ipv4_at_sentence_end_is_redacted() {
        let redacted = Redactor.redact("connected to 10.1.2.3.");
        assert_eq!(redacted, "connected to [REDACTED_IP].");
    }

    #[test]
    fn structured_secret_names_mask_non_textual_looking_values() {
        assert_eq!(
            Redactor.redact_field("stream_key", "12345"),
            Redactor::SECRET_MARKER
        );
        assert_eq!(Redactor.redact("frame=12345"), "frame=12345");
    }
}
