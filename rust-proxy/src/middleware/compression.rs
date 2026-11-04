use crate::middleware::middlewares::Middleware;
use crate::vojo::app_error::AppError;
use async_trait::async_trait;
use bytes::Bytes;
use http::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
use http::{HeaderMap, Response};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use std::io::Write;
use tracing::{debug, warn};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum CompressionType {
    #[default]
    Gzip,
    Zstd,
    #[serde(alias = "both")]
    Both,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Compression {
    #[serde(default)]
    pub compression_type: CompressionType,
    #[serde(default = "default_level")]
    pub level: i32,
    #[serde(default = "default_min_size")]
    pub min_size: usize,
    /// Largest response that may be buffered for compression. Bodies are
    /// buffered in memory to be compressed, so an unbounded (chunked) or huge
    /// response must be passed through untouched instead.
    #[serde(default = "default_max_size")]
    pub max_size: usize,
    #[serde(default = "default_excluded_types")]
    pub excluded_content_types: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
}

fn default_level() -> i32 {
    6 // 默认压缩级别 (0-9 for gzip, 1-22 for zstd)
}

fn default_min_size() -> usize {
    1024 // 默认最小压缩大小 1KB
}

fn default_max_size() -> usize {
    1024 * 1024 // 默认最大压缩大小 1MB
}

fn default_excluded_types() -> Vec<String> {
    vec![
        "image/png".to_string(),
        "image/jpeg".to_string(),
        "image/gif".to_string(),
        "image/webp".to_string(),
        "image/svg+xml".to_string(),
        "video/".to_string(),
        "audio/".to_string(),
        "application/zip".to_string(),
        "application/gzip".to_string(),
        "application/x-gzip".to_string(),
        "application/x-zip-compressed".to_string(),
        "application/wasm".to_string(),
    ]
}

impl Default for Compression {
    fn default() -> Self {
        Self {
            compression_type: CompressionType::default(),
            level: default_level(),
            min_size: default_min_size(),
            max_size: default_max_size(),
            excluded_content_types: default_excluded_types(),
            enabled: true,
        }
    }
}

impl Compression {
    /// 检查是否应该压缩此内容类型
    pub fn should_compress(&self, content_type: Option<&str>) -> bool {
        if !self.enabled {
            return false;
        }

        // 检查内容类型是否在排除列表中
        if let Some(ct) = content_type {
            for excluded in &self.excluded_content_types {
                if ct.starts_with(excluded) {
                    return false;
                }
            }
        }

        true
    }

    /// 解析 Accept-Encoding 头部，选择最佳压缩算法
    pub fn parse_accept_encoding(&self, headers: &HeaderMap) -> Option<CompressionType> {
        let accept_encoding = headers.get(ACCEPT_ENCODING)?.to_str().ok()?;

        // 检查支持的压缩格式
        let supports_gzip = accept_encoding.contains("gzip") || accept_encoding.contains("*");
        let supports_zstd = accept_encoding.contains("zstd") || accept_encoding.contains("*");

        match self.compression_type {
            CompressionType::Gzip if supports_gzip => Some(CompressionType::Gzip),
            CompressionType::Zstd if supports_zstd => Some(CompressionType::Zstd),
            CompressionType::Both => {
                // 优先使用 zstd (更好的压缩率)，其次是 gzip (更广泛的兼容性)
                if supports_zstd {
                    Some(CompressionType::Zstd)
                } else if supports_gzip {
                    Some(CompressionType::Gzip)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// 压缩数据
    pub fn compress_data(
        &self,
        data: &[u8],
        compression_type: &CompressionType,
    ) -> Result<Vec<u8>, AppError> {
        match compression_type {
            CompressionType::Gzip => {
                use flate2::Compression;
                use flate2::write::GzEncoder;

                let level = self.level.clamp(0, 9) as u32;
                let mut encoder = GzEncoder::new(Vec::new(), Compression::new(level));
                encoder
                    .write_all(data)
                    .map_err(|e| AppError(format!("Gzip compression failed: {}", e)))?;
                encoder
                    .finish()
                    .map_err(|e| AppError(format!("Gzip finish failed: {}", e)))
            }
            CompressionType::Zstd => {
                use zstd::stream::write::Encoder as ZstdEncoder;

                let level = self.level.clamp(1, 22);
                let mut encoder = ZstdEncoder::new(Vec::new(), level)
                    .map_err(|e| AppError(format!("Zstd encoder creation failed: {}", e)))?;
                encoder
                    .write_all(data)
                    .map_err(|e| AppError(format!("Zstd compression failed: {}", e)))?;
                encoder
                    .finish()
                    .map_err(|e| AppError(format!("Zstd finish failed: {}", e)))
            }
            CompressionType::Both => {
                // 这种情况不应该发生，因为 Both 在解析时会被转换为具体的类型
                Ok(data.to_vec())
            }
        }
    }

    /// 获取编码头部值
    pub fn get_encoding_value(&self, compression_type: &CompressionType) -> &'static str {
        match compression_type {
            CompressionType::Gzip => "gzip",
            CompressionType::Zstd => "zstd",
            CompressionType::Both => "gzip",
        }
    }

    /// 检查响应是否应该被压缩
    pub fn should_compress_response(
        &self,
        response: &Response<BoxBody<Bytes, AppError>>,
        request_headers: &HeaderMap,
    ) -> Option<CompressionType> {
        if !self.enabled {
            return None;
        }

        // 检查客户端支持的压缩格式
        let compression_type = self.parse_accept_encoding(request_headers)?;

        // 检查是否已经有 Content-Encoding
        if response.headers().contains_key(CONTENT_ENCODING) {
            return None;
        }

        // 获取内容类型
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok());

        // 检查是否应该压缩此内容类型
        if !self.should_compress(content_type) {
            return None;
        }

        // The body is collected into memory before being compressed, so only
        // compress when the size is known up front and within [min_size, max_size].
        // Without this a single large (or chunked, unknown length) response is
        // buffered whole: a trivial memory exhaustion vector, and it also breaks
        // streaming.
        let content_length = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok());
        let Some(content_length) = content_length else {
            debug!(
                "[Compression] Response has no usable Content-Length, passing it through uncompressed"
            );
            return None;
        };
        if content_length < self.min_size {
            debug!(
                "[Compression] Response of {content_length} bytes is below min_size {}, skipping",
                self.min_size
            );
            return None;
        }
        if content_length > self.max_size {
            debug!(
                "[Compression] Response of {content_length} bytes exceeds max_size {}, skipping",
                self.max_size
            );
            return None;
        }

        Some(compression_type)
    }
}

#[async_trait]
impl Middleware for Compression {
    async fn handle_response(
        &self,
        _req_path: &str,
        response: &mut Response<BoxBody<Bytes, AppError>>,
        inbound_headers: HeaderMap,
    ) -> Result<(), AppError> {
        debug!("[Compression] Checking if response should be compressed");

        // 检查是否应该压缩
        let compression_type = match self.should_compress_response(response, &inbound_headers) {
            Some(ct) => ct,
            None => {
                debug!("[Compression] Compression not applicable for this response");
                return Ok(());
            }
        };

        debug!(
            "[Compression] Response approved for compression with type: {:?}",
            compression_type
        );

        // 打印原始响应头
        debug!("[Compression] Original response headers:");
        for (name, value) in response.headers().iter() {
            if let Ok(v_str) = value.to_str() {
                debug!("[Compression]   {}: {}", name, v_str);
            } else {
                debug!("[Compression]   {}: {:?}", name, value);
            }
        }
        debug!("[Compression] Response status: {}", response.status());

        // 收集响应体
        let body = std::mem::take(response.body_mut());
        let collected = body
            .collect()
            .await
            .map_err(|e| AppError(format!("Failed to collect response body: {}", e)))?;
        let data = collected.to_bytes();

        // 检查数据大小是否达到最小压缩要求
        debug!(
            "[Compression] Response body size: {} bytes, min_size: {} bytes",
            data.len(),
            self.min_size
        );
        if data.len() < self.min_size {
            // 恢复原始响应体
            *response.body_mut() = Full::new(data).map_err(AppError::from).boxed();
            debug!("[Compression] Body size too small, skipping compression");
            return Ok(());
        }

        // 压缩数据
        let compressed_data = self.compress_data(&data, &compression_type)?;

        // 只有在压缩后数据更小时才使用压缩版本
        if compressed_data.len() >= data.len() {
            let data_len = data.len();
            // 恢复原始响应体
            *response.body_mut() = Full::new(data).map_err(AppError::from).boxed();
            warn!(
                "[Compression] Compressed size ({}) >= original size ({}), using original",
                compressed_data.len(),
                data_len
            );
            return Ok(());
        }

        // 移除 Content-Length，添加 Content-Encoding
        response.headers_mut().remove(CONTENT_LENGTH);
        response.headers_mut().insert(
            CONTENT_ENCODING,
            http::HeaderValue::from_static(self.get_encoding_value(&compression_type)),
        );

        // 替换响应体
        *response.body_mut() = Full::new(Bytes::from(compressed_data))
            .map_err(AppError::from)
            .boxed();

        debug!("[Compression] Compression applied successfully");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::HeaderMap;

    #[test]
    fn test_compression_type_default() {
        let ct = CompressionType::default();
        assert_eq!(ct, CompressionType::Gzip);
    }

    #[test]
    fn test_compression_default() {
        let comp = Compression::default();
        assert_eq!(comp.compression_type, CompressionType::Gzip);
        assert_eq!(comp.level, 6);
        assert_eq!(comp.min_size, 1024);
        assert!(comp.enabled);
    }

    #[test]
    fn test_should_compress_excluded_types() {
        let comp = Compression::default();

        // 这些类型应该被排除
        assert!(!comp.should_compress(Some("image/png")));
        assert!(!comp.should_compress(Some("image/jpeg")));
        assert!(!comp.should_compress(Some("video/mp4")));
        assert!(!comp.should_compress(Some("application/zip")));

        // 这些类型应该被压缩
        assert!(comp.should_compress(Some("text/html")));
        assert!(comp.should_compress(Some("application/json")));
        assert!(comp.should_compress(Some("text/css")));
        assert!(comp.should_compress(Some("application/javascript")));
    }

    #[test]
    fn test_should_skip_compression_when_size_is_unknown_or_too_large() {
        let comp = Compression::default();
        let mut request_headers = HeaderMap::new();
        request_headers.insert(ACCEPT_ENCODING, "gzip".parse().unwrap());

        // Unknown length (chunked/streamed): the body must not be buffered.
        let response = Response::builder()
            .header(CONTENT_TYPE, "application/json")
            .body(BoxBody::default())
            .unwrap();
        assert_eq!(
            comp.should_compress_response(&response, &request_headers),
            None
        );

        // Larger than max_size: pass through instead of buffering it whole.
        let response = Response::builder()
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_LENGTH, (comp.max_size + 1).to_string())
            .body(BoxBody::default())
            .unwrap();
        assert_eq!(
            comp.should_compress_response(&response, &request_headers),
            None
        );

        // Within [min_size, max_size]: compressed.
        let response = Response::builder()
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_LENGTH, (comp.min_size + 1).to_string())
            .body(BoxBody::default())
            .unwrap();
        assert_eq!(
            comp.should_compress_response(&response, &request_headers),
            Some(CompressionType::Gzip)
        );
    }
    #[test]
    fn test_parse_accept_encoding() {
        let comp = Compression::default();
        assert_eq!(comp.compression_type, CompressionType::Gzip);

        // 测试 gzip
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT_ENCODING, "gzip".parse().unwrap());
        assert_eq!(
            comp.parse_accept_encoding(&headers),
            Some(CompressionType::Gzip)
        );

        // A gzip-only configuration must not negotiate zstd, even if the
        // client accepts it.
        headers.insert(ACCEPT_ENCODING, "zstd".parse().unwrap());
        assert_eq!(comp.parse_accept_encoding(&headers), None);

        // 测试 zstd
        let comp_zstd = Compression {
            compression_type: CompressionType::Zstd,
            ..Default::default()
        };
        assert_eq!(
            comp_zstd.parse_accept_encoding(&headers),
            Some(CompressionType::Zstd)
        );
        headers.insert(ACCEPT_ENCODING, "gzip".parse().unwrap());
        assert_eq!(comp_zstd.parse_accept_encoding(&headers), None);

        // 测试两者都支持
        let comp_both = Compression {
            compression_type: CompressionType::Both,
            ..Default::default()
        };
        headers.insert(ACCEPT_ENCODING, "gzip, deflate, br".parse().unwrap());
        assert_eq!(
            comp_both.parse_accept_encoding(&headers),
            Some(CompressionType::Gzip)
        );

        headers.insert(ACCEPT_ENCODING, "zstd, gzip".parse().unwrap());
        assert_eq!(
            comp_both.parse_accept_encoding(&headers),
            Some(CompressionType::Zstd)
        );

        // 测试通配符
        headers.insert(ACCEPT_ENCODING, "*".parse().unwrap());
        assert!(comp.parse_accept_encoding(&headers).is_some());
    }

    #[test]
    fn test_compress_data_gzip() {
        let comp = Compression::default();
        let data = b"Hello, World! ".repeat(100); // 创建一些重复数据以便更好地压缩

        let compressed = comp.compress_data(&data, &CompressionType::Gzip).unwrap();

        // 压缩后的数据应该更小
        assert!(compressed.len() < data.len());
    }

    #[test]
    fn test_compress_data_zstd() {
        let comp = Compression {
            compression_type: CompressionType::Zstd,
            level: 3,
            ..Default::default()
        };
        let data = b"Hello, World! ".repeat(100);

        let compressed = comp.compress_data(&data, &CompressionType::Zstd).unwrap();

        // 压缩后的数据应该更小
        assert!(compressed.len() < data.len());
    }

    #[test]
    fn test_compress_disabled() {
        let comp = Compression {
            enabled: false,
            ..Default::default()
        };

        assert!(!comp.should_compress(Some("text/html")));
    }
}
