//! Request-body compression for the capture pipelines.
//!
//! `gzip` is always available (used by V0 and V1). The multi-algorithm
//! `compress` entry point is V1-only, since deflate/br/zstd ship behind the
//! `capture-v1` feature.

#[cfg(feature = "capture-v1")]
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

/// Compress `data` with `algo`, returning the compressed bytes alongside the
/// HTTP `Content-Encoding` token to advertise. Returns `None` when compression
/// fails, signalling the caller to send the payload uncompressed without a
/// `Content-Encoding` header.
#[cfg(feature = "capture-v1")]
pub(crate) fn compress(algo: CaptureCompression, data: &[u8]) -> Option<(Vec<u8>, &'static str)> {
    use std::io::Write;

    let encoding = algo.content_encoding();
    if let CaptureCompression::Gzip = algo {
        return gzip(data).map(|bytes| (bytes, encoding));
    }
    let result: std::io::Result<Vec<u8>> = match algo {
        CaptureCompression::Gzip => unreachable!("handled above"),
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
    fn gzip_helper_roundtrips() {
        let data = br#"{"hello":"world"}"#;
        let compressed = gzip(data).unwrap();
        assert_ne!(compressed, data);

        let mut decoder = flate2::read::GzDecoder::new(&compressed[..]);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
    }

    #[cfg(feature = "capture-v1")]
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

    #[cfg(feature = "capture-v1")]
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

    #[cfg(feature = "capture-v1")]
    #[test]
    fn zstd_roundtrips() {
        let data = br#"{"hello":"world"}"#;
        let (compressed, encoding) = compress(CaptureCompression::Zstd, data).unwrap();
        assert_eq!(encoding, "zstd");
        let out = zstd::stream::decode_all(&compressed[..]).unwrap();
        assert_eq!(out, data);
    }

    #[cfg(feature = "capture-v1")]
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
