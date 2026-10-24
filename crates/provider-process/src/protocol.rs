use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{BufRead, Write},
    path::PathBuf,
};
use tradeslope_sdk::runtime::{AccountProfile, ExtensionSettings};
use tradeslope_sdk::{BackendError, BackendManifest, BackendResult, DataSource};

pub const VERSION: u32 = 1;
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
// Envelope overhead is bounded separately from execute parameters.
pub const MAX_REQUEST_BYTES: usize = tradeslope_sdk::MAX_PARAMS_BYTES + 64 * 1024;

pub fn failure(code: &str) -> BackendError {
    BackendError::new(code, "Provider process request failed")
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub settings: Value,
    pub accounts: Vec<AccountProfile>,
    pub selected: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub manifest: BackendManifest,
    pub sources: Vec<DataSource>,
    pub settings: ExtensionSettings,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub id: u64,
    #[serde(flatten)]
    pub method: Method,
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Method {
    Metadata,
    Initialize { db_path: PathBuf, context: Context },
    Context(Context),
    Execute { operation: String, params: Value },
    Shutdown,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Frame {
    Response {
        version: u32,
        id: u64,
        result: Result<Value, BackendError>,
    },
    Changed {
        version: u32,
    },
}

/// Read without ever allocating more than the configured frame bound.
pub fn read<T: serde::de::DeserializeOwned>(
    reader: &mut impl BufRead,
    limit: usize,
) -> BackendResult<Option<T>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().map_err(|_| failure("process_io"))?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(failure("invalid_frame"))
            };
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(available.len());
        if bytes.len() + count > limit {
            return Err(failure("frame_too_large"));
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if bytes.last() == Some(&b'\n') {
            return serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| failure("invalid_frame"));
        }
    }
}

pub fn encode(value: &impl Serialize, limit: usize) -> BackendResult<Vec<u8>> {
    struct Bounded(Vec<u8>, usize);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len() + bytes.len() >= self.1 {
                return Err(std::io::Error::other("frame limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut bytes = Bounded(Vec::new(), limit);
    serde_json::to_writer(&mut bytes, value).map_err(|_| failure("frame_too_large"))?;
    bytes.0.push(b'\n');
    Ok(bytes.0)
}

pub fn write(writer: &mut impl Write, value: &impl Serialize, limit: usize) -> BackendResult<()> {
    let bytes = encode(value, limit)?;
    writer
        .write_all(&bytes)
        .and_then(|_| writer.flush())
        .map_err(|_| failure("process_io"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn strict_request_roundtrip_and_extra_field_rejection() {
        let request = Request {
            version: VERSION,
            id: 1,
            method: Method::Execute {
                operation: "echo".into(),
                params: Value::Null,
            },
        };
        let bytes = encode(&request, MAX_REQUEST_BYTES).unwrap();
        let decoded: Request = read(&mut Cursor::new(bytes), MAX_REQUEST_BYTES)
            .unwrap()
            .unwrap();
        assert_eq!(decoded.id, 1);
        for input in [
            r#"{"version":1,"id":1,"method":"execute","params":{"operation":"echo","params":null,"secret":"never echo"}}"#,
            r#"{"version":1,"id":1,"method":"shutdown","extra":"never echo"}"#,
        ] {
            let bytes = format!("{input}\n");
            let error = read::<Request>(&mut Cursor::new(bytes), MAX_REQUEST_BYTES)
                .err()
                .unwrap();
            assert_eq!(error.code, "invalid_frame");
            assert!(!error.message.contains("never echo"));
        }
    }

    #[test]
    fn frame_limits_include_newline_and_reject_truncation() {
        assert!(
            read::<Value>(&mut Cursor::new(b"null\n"), 5)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            read::<Value>(&mut Cursor::new(b"null\n"), 4)
                .unwrap_err()
                .code,
            "frame_too_large"
        );
        assert_eq!(
            read::<Value>(&mut Cursor::new(b"null"), 5)
                .unwrap_err()
                .code,
            "invalid_frame"
        );
        assert!(read::<Value>(&mut Cursor::new(b""), 5).unwrap().is_none());
        assert_eq!(encode(&Value::Null, 5).unwrap(), b"null\n");
        assert!(encode(&Value::Null, 4).is_err());
    }
}
