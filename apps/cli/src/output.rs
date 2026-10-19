use serde_json::{json, Value};
use std::io::{self, Write};

#[derive(Clone, Copy)]
pub struct Output {
    pub json: bool,
}

impl Output {
    pub fn event(self, event: &str, data: Value) {
        if self.json {
            // One lock and one line per event also serializes receiver worker output.
            let _ = writeln!(io::stdout().lock(), "{}", envelope(event, data));
        } else {
            eprintln!("{event}: {data}");
        }
    }

    pub fn terminal(self, command: &str, result: &Result<(), Failure>) {
        let data = match result {
            Ok(()) => json!({"command":command,"ok":true,"exitCode":0}),
            Err(error) => {
                json!({"command":command,"ok":false,"exitCode":error.code,"category":error.category,"message":error.message})
            }
        };
        self.event("result", data);
    }
}

fn envelope(event: &str, data: Value) -> Value {
    json!({"schemaVersion":1,"event":event,"data":data})
}

#[derive(Debug)]
pub struct Failure {
    pub code: i32,
    pub category: &'static str,
    pub message: String,
}

impl Failure {
    pub fn new(code: i32, category: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            code,
            category,
            message: error.to_string(),
        }
    }
    pub fn policy(error: impl std::fmt::Display) -> Self {
        Self::new(2, "policy", error)
    }
    pub fn unavailable(error: impl std::fmt::Display) -> Self {
        Self::new(3, "unavailable", error)
    }
    pub fn operation(error: impl std::fmt::Display) -> Self {
        Self::new(5, "operation", error)
    }
    pub fn cancelled() -> Self {
        Self::new(130, "cancelled", "操作已取消")
    }
}

impl From<omdrop_core::SendError> for Failure {
    fn from(error: omdrop_core::SendError) -> Self {
        use omdrop_core::TransferFailureKind::*;
        let code = match error.kind() {
            Declined => 4,
            Cancelled => 130,
            _ => 5,
        };
        Self::new(code, error.kind().as_str(), error)
    }
}

impl From<omdrop_service::client::ClientError> for Failure {
    fn from(error: omdrop_service::client::ClientError) -> Self {
        use omdrop_service::client::ClientErrorKind::*;
        let code = match error.kind() {
            Busy => 6,
            InvalidRequest => 2,
            _ => 3,
        };
        Self::new(code, if code == 6 { "busy" } else { "service" }, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_is_one_json_line_with_a_version_and_stable_terminal_fields() {
        let event = envelope(
            "result",
            json!({"command":"send","ok":false,"exitCode":130,"message":"line\nbreak"}),
        );
        let encoded = event.to_string();
        assert!(!encoded.contains('\n'));
        let decoded: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded["schemaVersion"], 1);
        assert_eq!(decoded["event"], "result");
        assert_eq!(decoded["data"]["exitCode"], 130);
    }
}
