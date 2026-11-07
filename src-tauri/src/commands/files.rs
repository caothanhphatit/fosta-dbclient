//! Exports saved to the Downloads folder.

use crate::{
    error::{AppError, AppResult},
    files,
};

/// Saves an export into Downloads; `base64` is for binary content such as PNG.
#[tauri::command]
pub fn save_download(name: String, text: Option<String>, base64: Option<String>) -> AppResult<String> {
    let bytes = match (text, base64) {
        (Some(text), _) => text.into_bytes(),
        (None, Some(encoded)) => decode_base64(&encoded)?,
        (None, None) => return Err(AppError::new("Nothing to save")),
    };
    files::save_download(&name, &bytes)
}

#[tauri::command]
pub fn reveal_file(path: String) -> AppResult<()> {
    files::reveal(&path)
}

fn decode_base64(encoded: &str) -> AppResult<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (index, byte) in ALPHABET.iter().enumerate() {
        lookup[*byte as usize] = index as u8;
    }
    let mut output = Vec::with_capacity(encoded.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in encoded.bytes().filter(|byte| !byte.is_ascii_whitespace() && *byte != b'=') {
        let value = lookup[byte as usize];
        if value == 255 {
            return Err(AppError::new("Invalid base64 data"));
        }
        buffer = (buffer << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::decode_base64;

    #[test]
    fn decodes_padded_and_unpadded_base64() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("aGVsbG8").unwrap(), b"hello");
        assert_eq!(decode_base64("iVBO\nRw==").unwrap(), [0x89, 0x50, 0x4E, 0x47]);
        assert!(decode_base64("not*base64").is_err());
    }
}
