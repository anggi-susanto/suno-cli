use crate::errors::CliError;
use serde::Serialize;
use std::io::Write;

#[derive(Serialize)]
pub struct Envelope<T: Serialize> {
    pub version: &'static str,
    pub status: &'static str,
    pub data: T,
}

fn write_json<T: Serialize>(mut writer: impl Write, value: &T) -> Result<(), CliError> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    Ok(())
}
pub fn print<T: Serialize>(value: &T) -> Result<(), CliError> {
    write_json(std::io::stdout().lock(), value)
}
pub fn success<T: Serialize>(data: T) -> Result<(), CliError> {
    with_status("success", data)
}
pub fn with_status<T: Serialize>(status: &'static str, data: T) -> Result<(), CliError> {
    print(&Envelope {
        version: "1",
        status,
        data,
    })
}
pub fn help(usage: &str) -> Result<(), CliError> {
    success(serde_json::json!({"usage":usage}))
}
pub fn error(code: &str, message: &str, suggestion: &str) {
    error_details(code, message, suggestion, None);
}
pub fn error_details(
    code: &str,
    message: &str,
    suggestion: &str,
    details: Option<serde_json::Value>,
) {
    error_details_with_timings(code, message, suggestion, details, None);
}
pub fn error_details_with_timings(
    code: &str,
    message: &str,
    suggestion: &str,
    details: Option<serde_json::Value>,
    timings: Option<&serde_json::Value>,
) {
    let mut error = serde_json::json!({"code":code,"message":message,"suggestion":suggestion});
    if let Some(details) = details {
        error["details"] = details;
    }
    let mut envelope = serde_json::json!({"version":"1","status":"error","error":error});
    if let Some(timings) = timings {
        envelope["timings"] = timings.clone();
    }
    let _ = write_json(std::io::stderr().lock(), &envelope);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_pipe_returns_error_without_panic() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            write_json(Closed, &serde_json::json!({}))
                .unwrap_err()
                .exit_code(),
            1
        );
    }
    #[test]
    fn serialization_failure_writes_no_bytes() {
        struct Invalid;
        impl Serialize for Invalid {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("invalid"))
            }
        }
        let mut buffer = vec![];
        assert!(write_json(&mut buffer, &Invalid).is_err());
        assert!(buffer.is_empty());
    }
}
