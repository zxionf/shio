//! 随机标识、哈希与时间辅助。

use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::error::{RelayError, Result};

const HEX: &[u8; 16] = b"0123456789abcdef";

/// 生成 16 字节随机 hex 标识。
pub fn random_hex_id() -> Result<String> {
    Ok(hex(&random_bytes::<16>()?))
}

/// 生成 32 字节随机 base64url 秘密。
pub fn random_secret() -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes::<32>()?))
}

/// SHA-256 的 hex 表示。
pub fn sha256_hex(input: &[u8]) -> String {
    let digest = Sha256::digest(input);
    hex(digest.as_slice())
}

/// 小写 hex。
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// 当前 Unix 秒。
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|_| RelayError::InvalidArgument("random source failed"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
    }

    #[test]
    fn random_values_have_expected_shapes() {
        let id = random_hex_id().unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|byte| byte.is_ascii_hexdigit()));

        let secret = random_secret().unwrap();
        assert_eq!(secret.len(), 43);
        assert_ne!(secret, random_secret().unwrap());
    }

    #[test]
    fn sha256_is_stable() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
