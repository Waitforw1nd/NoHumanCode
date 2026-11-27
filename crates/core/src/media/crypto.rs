//! Pure-Rust cryptographic primitives for media adapters and WebSockets.
//!
//! Includes RFC 2104 HMAC-SHA256, HS256 JWT (including Kling AK/SK tokens),
//! SHA-256 hex digests, FIPS 180-4 SHA-1 (for RFC 6455 Sec-WebSocket-Accept),
//! and constant-time equality.

use super::{MediaError, MediaErrorCode};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Computes RFC 2104 HMAC-SHA256.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut normalized_key = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let hashed = Sha256::digest(key);
        normalized_key[..32].copy_from_slice(&hashed);
    } else {
        normalized_key[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK_SIZE];
    let mut opad = [0x5cu8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ipad[i] ^= normalized_key[i];
        opad[i] ^= normalized_key[i];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

/// Encodes an HS256 JWT (`{"alg":"HS256","typ":"JWT"}`).
pub fn encode_hs256_jwt<T: Serialize>(claims: &T, secret: &[u8]) -> Result<String, MediaError> {
    if secret.is_empty() {
        return Err(MediaError::invalid_request(
            "JWT signing secret must not be empty",
        ));
    }
    let header_b64 = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let claims_bytes = serde_json::to_vec(claims)
        .map_err(|e| MediaError::new(MediaErrorCode::InvalidRequest, e.to_string()))?;
    let claims_b64 = URL_SAFE_NO_PAD.encode(claims_bytes);
    let signing_input = format!("{header_b64}.{claims_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(sig);
    Ok(format!("{signing_input}.{sig_b64}"))
}

#[derive(Serialize)]
struct KlingJwtClaims<'a> {
    iss: &'a str,
    exp: i64,
    nbf: i64,
}

/// Encodes a Kling AI HS256 JWT with `iss = AK`, `exp = now + 1800`, `nbf = now - 5`.
pub fn encode_kling_jwt(
    access_key: &str,
    secret_key: &str,
    now_unix_secs: i64,
) -> Result<String, MediaError> {
    if access_key.trim().is_empty() || secret_key.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "Kling access_key and secret_key must not be empty",
        ));
    }
    let claims = KlingJwtClaims {
        iss: access_key,
        exp: now_unix_secs.saturating_add(1800),
        nbf: now_unix_secs.saturating_sub(5),
    };
    encode_hs256_jwt(&claims, secret_key.as_bytes())
}

pub fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

/// Pure-Rust FIPS 180-4 SHA-1 digest (used exclusively for RFC 6455 Sec-WebSocket-Accept).
pub fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x6745_2301;
    let mut h1: u32 = 0xEFCD_AB89;
    let mut h2: u32 = 0x98BA_DCFE;
    let mut h3: u32 = 0x1032_5476;
    let mut h4: u32 = 0xC3D2_E1F0;

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((data.len() + 9).div_ceil(64) * 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for (i, &word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDCu32),
                _ => (b ^ c ^ d, 0xCA62_C1D6u32),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    fn hex_encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn rfc_4231_hmac_sha256_test_vectors() {
        // Test Case 1
        let key1 = [0x0bu8; 20];
        let data1 = b"Hi There";
        assert_eq!(
            hex_encode(&hmac_sha256(&key1, data1)),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );

        // Test Case 2 ("Jefe")
        assert_eq!(
            hex_encode(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );

        // Test Case 6 (key > 64 bytes)
        let key6 = [0xaau8; 131];
        let data6 = b"Test Using Larger Than Block-Size Key - Hash Key First";
        assert_eq!(
            hex_encode(&hmac_sha256(&key6, data6)),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn kling_hs256_jwt_claims_and_signature_verify() {
        let token = encode_kling_jwt("ak-test-123", "sk-secret-456", 1_700_000_000).unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);

        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header, serde_json::json!({"alg": "HS256", "typ": "JWT"}));

        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "ak-test-123");
        assert_eq!(claims["exp"], 1_700_001_800);
        assert_eq!(claims["nbf"], 1_699_999_995);

        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let expected_sig =
            URL_SAFE_NO_PAD.encode(hmac_sha256(b"sk-secret-456", signing_input.as_bytes()));
        assert!(constant_time_eq(
            parts[2].as_bytes(),
            expected_sig.as_bytes()
        ));
    }

    #[test]
    fn rfc_6455_sha1_accept_vector() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let combined = format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        let accept = STANDARD.encode(sha1_digest(combined.as_bytes()));
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}
