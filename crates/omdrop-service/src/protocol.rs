use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    #[serde(alias = "Cmd")]
    pub cmd: String,
    #[serde(alias = "Port")]
    pub port: Option<String>,
    #[serde(alias = "Channel")]
    pub channel: Option<u16>,
    #[serde(alias = "Exe")]
    pub exe: Option<String>,
}

impl Request {
    pub fn new(cmd: impl Into<String>) -> Self {
        Self {
            cmd: cmd.into(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub state: String,
    pub port: Option<String>,
    pub channel: u16,
    #[serde(default)]
    pub target: Option<String>,
    pub awdl0: bool,
    pub error: Option<String>,
    pub enabled: bool,
}

impl Status {
    pub fn with_error(mut self, error: impl ToString) -> Self {
        self.error = Some(error.to_string());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_camel_case_and_pascal_case_requests() {
        for json in [
            r#"{"cmd":"set","port":"COM4","channel":6}"#,
            r#"{"Cmd":"set","Port":"COM4","Channel":6}"#,
        ] {
            let req: Request = serde_json::from_str(json).unwrap();
            assert_eq!(req.cmd, "set");
            assert_eq!(req.port.as_deref(), Some("COM4"));
        }
        let status = Status {
            state: "stopped".into(),
            port: None,
            channel: 6,
            target: None,
            awdl0: false,
            error: None,
            enabled: false,
        };
        let value = serde_json::to_value(status).unwrap();
        assert_eq!(value["state"], "stopped");
        assert!(value.get("State").is_none());
    }
}
