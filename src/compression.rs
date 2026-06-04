//! Request-body compression for the V1 capture pipeline.
//!
//! This module is only compiled when the `capture-v1` crate feature is enabled.

use crate::client::CaptureCompression;

/// Compress `data` with `algo`, returning the compressed bytes alongside the
/// HTTP `Content-Encoding` token to advertise. Returns `None` when compression
/// fails, signalling the caller to send the payload uncompressed without a
/// `Content-Encoding` header.
pub(crate) fn compress(algo: CaptureCompression, data: &[u8]) -> Option<(Vec<u8>, &'static str)> {
    use std::io::Write;

    let encoding = algo.content_encoding();
    let result: std::io::Result<Vec<u8>> = match algo {
        CaptureCompression::Gzip => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(data).and_then(|_| encoder.finish())
        }
        CaptureCompression::Deflate => {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(data).and_then(|_| encoder.finish())
        }
        CaptureCompression::Br => {
            let mut out = Vec::new();
            {
                let mut encoder = brotli::CompressorWriter::new(&mut out, 4096, 5, 22);
                if let Err(e) = encoder.write_all(data).and_then(|_| encoder.flush()) {
                    Err(e)
                } else {
                    Ok(())
                }
            }
            .map(|_| out)
        }
        CaptureCompression::Zstd => zstd::stream::encode_all(data, 0),
    };

    match result {
        Ok(bytes) => Some((bytes, encoding)),
        Err(e) => {
            tracing::warn!(error = %e, encoding, "failed to compress V1 capture body; sending uncompressed");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn gzip_roundtrips() {
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
