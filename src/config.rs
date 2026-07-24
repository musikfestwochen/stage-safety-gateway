//! Configuration model, persistence, and validation.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub serial: SerialConfig,
    pub aggregation: AggregationConfig,
    #[serde(rename = "sensor", default)]
    pub sensors: Vec<Sensor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SerialConfig {
    pub port: String,
    pub baud_rate: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AggregationConfig {
    /// Relative change from the last sent value (percent) that triggers an
    /// immediate send, per sensor and reading kind.
    pub change_percent: f64,
    /// Minimum seconds between sends per sensor and reading kind (rate limit).
    pub min_interval_secs: u64,
    /// Maximum seconds between sends per sensor and reading kind (heartbeat).
    pub max_interval_secs: u64,
}

/// Sensor configuration, tagged by `type` in TOML. New sensor kinds become new
/// variants; deserialization rejects unknown types loudly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Sensor {
    BwWss(BwWssSensor),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BwWssSensor {
    /// Free-form label (toolkit `Name`), used in logs and prompts.
    #[serde(default)]
    pub name: String,
    /// Hardware ID from the toolkit (`Information.ID`), 6 hex characters.
    pub id: String,
    pub unit: WindUnit,
    /// Base data tag; gust frames arrive on `data_tag + 1`.
    #[serde(with = "hex_tag")]
    pub data_tag: u16,
    #[serde(default)]
    pub gust: bool,
    pub average_window_secs: u64,
    /// Required (and only meaningful) when `gust` is enabled.
    #[serde(default)]
    pub gust_window_secs: u64,
    pub url: String,
    pub token: String,
}

/// Default Musikfestapp ingestion endpoint, prefilled in the wizard.
pub const DEFAULT_URL: &str = "https://musikfestapp.ch/stage-safety/readings";

impl Sensor {
    /// One-line detail view. Never prints the token.
    pub fn details(&self) -> String {
        match self {
            Sensor::BwWss(s) => format!(
                "{} [bw-wss] id={} tag={:04X} unit={} avg={}s gust={} url={} token={}",
                s.name,
                s.id,
                s.data_tag,
                s.unit.as_str(),
                s.average_window_secs,
                if s.gust {
                    format!("{}s", s.gust_window_secs)
                } else {
                    "off".to_string()
                },
                s.url,
                if s.token.is_empty() {
                    "NOT SET"
                } else {
                    "********"
                }
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WindUnit {
    #[serde(rename = "m/s")]
    Mps,
    #[serde(rename = "km/h")]
    Kmh,
    #[serde(rename = "mph")]
    Mph,
    #[serde(rename = "fps")]
    Fps,
    #[serde(rename = "kn")]
    Kn,
}

impl std::fmt::Display for WindUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl WindUnit {
    pub const ALL: [WindUnit; 5] = [
        WindUnit::Mps,
        WindUnit::Kmh,
        WindUnit::Mph,
        WindUnit::Fps,
        WindUnit::Kn,
    ];

    /// String form expected by the Musikfestapp ingestion contract.
    pub fn as_str(self) -> &'static str {
        match self {
            WindUnit::Mps => "m/s",
            WindUnit::Kmh => "km/h",
            WindUnit::Mph => "mph",
            WindUnit::Fps => "fps",
            WindUnit::Kn => "kn",
        }
    }
}

/// Serializes the data tag as uppercase hex (`"25DF"`) to match toolkit exports.
mod hex_tag {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(tag: &u16, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{tag:04X}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
        let s = String::deserialize(deserializer)?;
        u16::from_str_radix(&s, 16).map_err(serde::de::Error::custom)
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read config file {}", path.display()))?;
        let config: Config = toml::from_str(&text)
            .with_context(|| format!("cannot parse config file {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create directory {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("cannot serialize config")?;
        fs::write(path, text)
            .with_context(|| format!("cannot write config file {}", path.display()))?;
        // Config contains API tokens.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        let mut problems = Vec::new();
        if self.serial.port.trim().is_empty() {
            problems.push("serial.port is empty".to_string());
        }
        if self.serial.baud_rate == 0 {
            problems.push("serial.baud_rate must be positive".to_string());
        }
        if self.aggregation.change_percent < 0.0 {
            problems.push("aggregation.change_percent must be >= 0".to_string());
        }
        if self.aggregation.max_interval_secs == 0 {
            problems.push("aggregation.max_interval_secs must be positive".to_string());
        }
        if self.aggregation.min_interval_secs > self.aggregation.max_interval_secs {
            problems.push(
                "aggregation.min_interval_secs must not exceed max_interval_secs".to_string(),
            );
        }
        if self.sensors.is_empty() {
            problems.push("at least one [[sensor]] is required".to_string());
        }

        let mut tags = Vec::new();
        for sensor in &self.sensors {
            let Sensor::BwWss(s) = sensor;
            if s.id.len() != 6 || !s.id.chars().all(|c| c.is_ascii_hexdigit()) {
                problems.push(format!("sensor {:?}: id must be 6 hex characters", s.name));
            }
            if !s.url.starts_with("http://") && !s.url.starts_with("https://") {
                problems.push(format!(
                    "sensor {:?}: url must start with http(s)://",
                    s.name
                ));
            }
            if s.token.is_empty() {
                problems.push(format!("sensor {:?}: token is empty", s.name));
            }
            if s.average_window_secs == 0 {
                problems.push(format!(
                    "sensor {:?}: average_window_secs must be positive",
                    s.name
                ));
            }
            if s.gust && s.gust_window_secs == 0 {
                problems.push(format!(
                    "sensor {:?}: gust_window_secs is required when gust is enabled",
                    s.name
                ));
            }
            if tags.contains(&s.data_tag) {
                problems.push(format!(
                    "sensor {:?}: data_tag {:04X} is used by another sensor",
                    s.name, s.data_tag
                ));
            }
            tags.push(s.data_tag);
        }

        if problems.is_empty() {
            Ok(())
        } else {
            bail!("invalid configuration:\n - {}", problems.join("\n - "))
        }
    }

    /// Redacted, human-readable summary. Never prints tokens.
    pub fn summary(&self) -> String {
        let mut out = format!(
            "serial: {} @ {}\naggregation: change_percent={} min_interval={}s max_interval={}s\nsensors ({}):",
            self.serial.port,
            self.serial.baud_rate,
            self.aggregation.change_percent,
            self.aggregation.min_interval_secs,
            self.aggregation.max_interval_secs,
            self.sensors.len()
        );
        for sensor in &self.sensors {
            out.push_str(&format!("\n  - {}", sensor.details()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_config() -> Config {
        Config {
            serial: SerialConfig {
                port: "/dev/ttyUSB0".into(),
                baud_rate: 115200,
            },
            aggregation: AggregationConfig {
                change_percent: 20.0,
                min_interval_secs: 30,
                max_interval_secs: 300,
            },
            sensors: vec![Sensor::BwWss(BwWssSensor {
                name: "WINDMESSER1".into(),
                id: "1A2B3F".into(),
                unit: WindUnit::Mps,
                data_tag: 0x25DF,
                gust: true,
                average_window_secs: 10,
                gust_window_secs: 10,
                url: "https://musikfest.example".into(),
                token: "secret".into(),
            })],
        }
    }

    #[test]
    fn toml_roundtrip_uses_hex_tag_and_type_tag() {
        let text = toml::to_string_pretty(&example_config()).unwrap();
        assert!(text.contains("type = \"bw-wss\""), "{text}");
        assert!(text.contains("data_tag = \"25DF\""), "{text}");
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed, example_config());
        parsed.validate().unwrap();
    }

    #[test]
    fn unknown_sensor_type_is_rejected() {
        let text = r#"
            [serial]
            port = "/dev/ttyUSB0"
            baud_rate = 115200
            [aggregation]
            change_percent = 20.0
            min_interval_secs = 30
            max_interval_secs = 300
            [[sensor]]
            type = "bw-scale"
        "#;
        let error = toml::from_str::<Config>(text).unwrap_err();
        assert!(error.to_string().contains("bw-scale"), "{error}");
    }

    #[test]
    fn validation_catches_problems() {
        let mut config = example_config();
        config.aggregation.min_interval_secs = config.aggregation.max_interval_secs + 1;
        let Sensor::BwWss(s) = &mut config.sensors[0];
        s.id = "ZZZ".into();
        s.url = "musikfest.example".into();
        s.token = String::new();
        s.gust_window_secs = 0;
        config.sensors.push(config.sensors[0].clone()); // duplicate tag
        let message = config.validate().unwrap_err().to_string();
        for expected in [
            "6 hex characters",
            "http(s)://",
            "token is empty",
            "gust_window_secs",
            "another sensor",
            "min_interval_secs must not exceed",
        ] {
            assert!(
                message.contains(expected),
                "missing {expected:?} in {message}"
            );
        }
    }

    #[test]
    fn summary_never_contains_token() {
        let summary = example_config().summary();
        assert!(summary.contains("********"), "{summary}");
        assert!(!summary.contains("secret"), "{summary}");
    }
}
