//! Request validation helpers for security-sensitive API inputs.

pub const MAX_DEVICE_NAME_LEN: usize = 128;
pub const MAX_PLATFORM_LEN: usize = 32;
pub const ED25519_PUBLIC_KEY_HEX_LEN: usize = 64;
pub const ED25519_SIGNATURE_HEX_LEN: usize = 128;

pub fn bounded_text(value: &str, max_len: usize) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && trimmed.len() <= max_len
}

pub fn is_hex_of_len(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|b| b.is_ascii_hexdigit())
}
