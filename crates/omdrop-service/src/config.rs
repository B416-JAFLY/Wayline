use serde::{Deserialize, Serialize};
use std::{fs, io, io::Write, path::Path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    #[serde(alias = "Port")]
    pub port: String,
    #[serde(alias = "Channel")]
    pub channel: u16,
    #[serde(alias = "Enabled")]
    pub enabled: bool,
    #[serde(alias = "OwlPath")]
    pub owl_path: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: "auto".into(),
            channel: 6,
            enabled: true,
            owl_path: None,
        }
    }
}

impl Config {
    pub fn validate(&mut self) -> io::Result<()> {
        self.port = normalize_port(&self.port)?;
        // OWL's AWDL channel table and the ESP32 firmware support these channels.
        if !matches!(self.channel, 6 | 44 | 149) {
            return Err(invalid(
                "channel must be 6 (ESP32-S3), 44 or 149 (ESP32-C5)",
            ));
        }
        if let Some(path) = &self.owl_path {
            if path.is_empty() || path.contains('\0') || !Path::new(path).is_absolute() {
                return Err(invalid("owlPath must be an absolute executable path"));
            }
        }
        Ok(())
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
        let mut config: Self = serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut valid = self.clone();
        valid.validate()?;
        let parent = path
            .parent()
            .ok_or_else(|| invalid("config path has no parent"))?;
        fs::create_dir_all(parent)?;
        // Persist uses an atomic replacement on Windows too; a failed write cannot truncate
        // the previous config. The temporary file stays on the same volume.
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, &valid)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
}

pub fn normalize_port(port: &str) -> io::Result<String> {
    let port = port.trim().to_ascii_uppercase();
    if port == "AUTO" {
        return Ok("auto".into());
    }
    if let Some(number) = port.strip_prefix("COM") {
        if !number.is_empty() && number.bytes().all(|c| c.is_ascii_digit()) {
            if let Ok(value @ 1..=65535) = number.parse::<u16>() {
                return Ok(format!("COM{value}"));
            }
        }
    }
    Err(invalid(
        "port must be auto or COM followed by a positive port number",
    ))
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pascal_case_config_with_utf8_bom_then_replaces_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            b"\xef\xbb\xbf{\"Port\":\"com004\",\"Channel\":44,\"Enabled\":false,\"OwlPath\":null}",
        )
        .unwrap();
        let mut config = Config::load(&path).unwrap();
        assert_eq!(config.port, "COM4");
        assert!(!config.enabled);
        config.channel = 149;
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
        let previous = fs::read(&path).unwrap();
        config.channel = 7;
        assert!(config.save(&path).is_err());
        assert_eq!(fs::read(path).unwrap(), previous);
    }

    #[test]
    fn missing_defaults_but_invalid_config_is_not_silently_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert_eq!(Config::load(&path).unwrap(), Config::default());
        fs::write(&path, "{bad").unwrap();
        assert!(Config::load(&path).is_err());
        for port in [
            "COM0",
            "COM-1",
            "COM65536",
            "COM1 -x",
            "COM¹",
            "\\\\.\\pipe\\foo",
        ] {
            assert!(normalize_port(port).is_err(), "{port}");
        }
    }
}
