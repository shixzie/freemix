//! Canonical unpadded RFC 4648 base64url, shared by the compact domain codes.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Encodes `data` as canonical unpadded base64url.
pub(crate) fn encode(data: &[u8]) -> String {
    let mut output = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let value = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |value, (index, byte)| {
                value | u32::from(*byte) << (16 - 8 * index)
            });
        for index in 0..=chunk.len() {
            let sextet = (value >> (18 - 6 * index)) & 0x3f;
            output.push(char::from(ALPHABET[sextet as usize]));
        }
    }
    output
}

/// Decodes canonical unpadded base64url; padding, standard alphabet
/// symbols, and trailing-bit noise are refused.
pub(crate) fn decode(text: &str) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.as_bytes().chunks(4) {
        if chunk.len() == 1 {
            return None;
        }
        let mut value = 0_u32;
        for (index, byte) in chunk.iter().enumerate() {
            let sextet = ALPHABET.iter().position(|candidate| candidate == byte)?;
            value |= u32::try_from(sextet).ok()? << (18 - 6 * index);
        }
        for index in 0..chunk.len() - 1 {
            output.push(u8::try_from((value >> (16 - 8 * index)) & 0xff).ok()?);
        }
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_follow_the_canonical_alphabet() {
        // Deliberately include every byte so all sextet values are exercised.
        let mut all = Vec::new();
        for byte in 0..=255_u8 {
            all.push(byte);
        }
        for length in 0..=all.len() {
            let raw = &all[..length];
            let text = encode(raw);
            assert_eq!(decode(&text).as_deref(), Some(raw), "{length}");
            assert!(!text.contains(['=', '+', '/']));
        }
        for invalid in ["A", "AA=A", "a+b/", "AA\n", "\u{00e9}"] {
            assert_eq!(decode(invalid), None, "{invalid:?}");
        }
    }
}
