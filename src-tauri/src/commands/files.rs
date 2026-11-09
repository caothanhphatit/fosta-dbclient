//! Exports saved to the Downloads folder.

use std::io::Write as _;

use super::gateway_context;
use crate::{
    error::{AppError, AppResult},
    export::{self, ChunkSink, ExportProgress, ExportRequest},
    files, AppState,
};

/// Saves an export into Downloads; `base64` is for binary content such as PNG.
pub fn save_download(name: String, text: Option<String>, base64: Option<String>) -> AppResult<String> {
    let bytes = match (text, base64) {
        (Some(text), _) => text.into_bytes(),
        (None, Some(encoded)) => decode_base64(&encoded)?,
        (None, None) => return Err(AppError::new("Nothing to save")),
    };
    files::save_download(&name, &bytes)
}

pub fn reveal_file(path: String) -> AppResult<()> {
    files::reveal(&path)
}

/// A finished export: where it was written and how many rows it holds.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportDone {
    pub path: String,
    pub rows: u64,
}

/// Writes the file in chunks as the pages arrive.
struct FileSink(std::io::BufWriter<std::fs::File>);

#[async_trait::async_trait]
impl ChunkSink for FileSink {
    async fn write(&mut self, chunk: Vec<u8>) -> AppResult<()> {
        self.0.write_all(&chunk)?;
        Ok(())
    }
}

/// Desktop: exports every row matching the grid into Downloads, page by page (`progress` after each page);
/// `cancel_request(request.request_id)` stops it and removes the partial file.
pub async fn export_rows(
    state: &AppState,
    connection_id: String,
    request: ExportRequest,
    mut progress: impl FnMut(ExportProgress) + Send,
) -> AppResult<ExportDone> {
    let context = gateway_context(state, &connection_id)?;
    let (cancel, _registered) = state.engines.requests.register(request.request_id.as_deref());
    let (file, path) = files::create_download(&request.file_name)?;
    let mut sink = FileSink(std::io::BufWriter::new(file));
    let mut on_page = |rows| progress(ExportProgress { rows });
    let written = export::export(&state.engines, &context, &connection_id, &request, &cancel, &mut sink, &mut on_page).await;
    let flushed = written.and_then(|rows| sink.0.flush().map(|()| rows).map_err(AppError::from));
    drop(sink);
    match flushed {
        Ok(rows) => Ok(ExportDone {
            path: path.to_string_lossy().into_owned(),
            rows,
        }),
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            Err(error)
        }
    }
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
