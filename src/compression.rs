//! Request-body compression for the capture pipeline.
//!
//! `compress` is the single entry point. All four codecs the capture endpoint
//! accepts — `gzip`, `deflate`, `br`, `zstd` — are always available; see
//! `SUPPORTED_ENCODINGS` in `rust/capture/src/v1/constants.rs`.

use crate::client::CaptureCompression;

/// Gzip-compress `data`. Returns `None` (with a warning) on failure so the
/// caller can fall back to sending the payload uncompressed.
pub(crate) fn gzip(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    match encoder.write_all(data).and_then(|_| encoder.finish()) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            tracing::warn!(error = %e, "failed to gzip capture body; sending uncompressed");
            None
        }
    }
}

fn deflate(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;

    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encode_or_warn(
        "deflate",
        encoder.write_all(data).and_then(|_| encoder.finish()),
    )
}

fn brotli(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;

    let mut out = Vec::new();
    let result = {
        let mut encoder = brotli::CompressorWriter::new(&mut out, 4096, 5, 22);
        encoder.write_all(data).and_then(|_| encoder.flush())
    };
    encode_or_warn("br", result.map(|_| out))
}

fn zstd(data: &[u8]) -> Option<Vec<u8>> {
    encode_or_warn("zstd", zstd::stream::encode_all(data, 0))
}

fn encode_or_warn(encoding: &'static str, result: std::io::Result<Vec<u8>>) -> Option<Vec<u8>> {
    match result {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            tracing::warn!(error = %e, encoding, "failed to compress capture body; sending uncompressed");
            None
        }
    }
}

/// Compress `data` with `algo`, returning the compressed bytes alongside the
/// HTTP `Content-Encoding` token to advertise. Returns `None` when compression
/// fails, signalling the caller to send the payload uncompressed without a
/// `Content-Encoding` header.
pub(crate) fn compress(algo: CaptureCompression, data: &[u8]) -> Option<(Vec<u8>, &'static str)> {
    let encoding = algo.content_encoding();
    let bytes = match algo {
        CaptureCompression::Gzip => gzip(data)?,
        CaptureCompression::Deflate => deflate(data)?,
        CaptureCompression::Br => brotli(data)?,
        CaptureCompression::Zstd => zstd(data)?,
    };
    Some((bytes, encoding))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn gzip_helper_roundtrips() {
        let data = br#"{"hello":"world"}"#;
        let compressed = gzip(data).unwrap();
        assert_ne!(compressed, data);

        let mut decoder = flate2::read::GzDecoder::new(&compressed[..]);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn compress_gzip_roundtrips() {
        let data = br#"{"hello":"world"}"#;
        let (compressed, encoding) = compress(CaptureCompression::Gzip, data).unwrap();
        assert_eq!(encoding, "gzip");
        assert_ne!(compressed, data);

        let mut decoder = flate2::read::GzDecoder::new(&compressed[..]);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn deflate_roundtrips() {
        let data = br#"{"hello":"world"}"#;
        let (compressed, encoding) = compress(CaptureCompression::Deflate, data).unwrap();
        assert_eq!(encoding, "deflate");

        let mut decoder = flate2::read::ZlibDecoder::new(&compressed[..]);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn zstd_roundtrips() {
        let data = br#"{"hello":"world"}"#;
        let (compressed, encoding) = compress(CaptureCompression::Zstd, data).unwrap();
        assert_eq!(encoding, "zstd");
        let out = zstd::stream::decode_all(&compressed[..]).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn brotli_produces_output() {
        let data = br#"{"hello":"world"}"#;
        let (compressed, encoding) = compress(CaptureCompression::Br, data).unwrap();
        assert_eq!(encoding, "br");
        let mut decoder = brotli::Decompressor::new(&compressed[..], 4096);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
    }
}
