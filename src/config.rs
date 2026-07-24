//! Configuration model, persistence, and validation.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::ReadingKind;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub serial: SerialConfig,
    pub aggregation: AggregationConfig,
    #[serde(rename = "sensor", default)]
    pub sensors: Vec<Sensor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SerialConfig {
    pub port: String,
    pub baud_rate: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

impl BwWssSensor {
    /// Identifies a sensor in messages: the configured name, or the hardware
    /// ID when `name` is left empty.
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

impl Sensor {
    /// One-line detail view. Never prints the token.
    pub fn details(&self) -> String {
        match self {
            Sensor::BwWss(s) => format!(
                "{} [bw-wss] id={} tag={:04X} unit={} avg={}s gust={} url={} token={}",
                s.display_name(),
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
    /// Parses a config file without validating it. The wizard uses this to
    /// open broken configs for repair; `load` is the strict variant.
    pub fn read(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read config file {}", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("cannot parse config file {}", path.display()))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let config = Self::read(path)?;
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
        // Config contains API tokens: create with 0600 from the start so the
        // file is never group/world-readable, then tighten pre-existing files.
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .and_then(|mut file| file.write_all(text.as_bytes()))
                .with_context(|| format!("cannot write config file {}", path.display()))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        #[cfg(not(unix))]
        fs::write(path, text)
            .with_context(|| format!("cannot write config file {}", path.display()))?;
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
        if !self.aggregation.change_percent.is_finite() || self.aggregation.change_percent < 0.0 {
            problems.push("aggregation.change_percent must be a finite number >= 0".to_string());
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

        // Tags occupied per sensor: base tag, plus base+1 when gust is enabled.
        let mut occupied: Vec<(u16, &str)> = Vec::new();
        for sensor in &self.sensors {
            let Sensor::BwWss(s) = sensor;
            if s.id.len() != 6 || !s.id.chars().all(|c| c.is_ascii_hexdigit()) {
                problems.push(format!(
                    "sensor {:?}: id must be 6 hex characters",
                    s.display_name()
                ));
            }
            if !s.url.starts_with("http://") && !s.url.starts_with("https://") {
                problems.push(format!(
                    "sensor {:?}: url must start with http(s)://",
                    s.display_name()
                ));
            }
            if s.token.is_empty() {
                problems.push(format!("sensor {:?}: token is empty", s.display_name()));
            }
            if s.average_window_secs == 0 {
                problems.push(format!(
                    "sensor {:?}: average_window_secs must be positive",
                    s.display_name()
                ));
            }
            let mut tags = vec![s.data_tag];
            if s.gust {
                if s.gust_window_secs == 0 {
                    problems.push(format!(
                        "sensor {:?}: gust_window_secs is required when gust is enabled",
                        s.display_name()
                    ));
                }
                match s.data_tag.checked_add(1) {
                    Some(gust_tag) => tags.push(gust_tag),
                    None => problems.push(format!(
                        "sensor {:?}: data_tag FFFF leaves no room for the gust tag",
                        s.display_name()
                    )),
                }
            }
            for tag in tags {
                if let Some((_, owner)) = occupied.iter().find(|(t, _)| *t == tag) {
                    problems.push(format!(
                        "sensor {:?}: tag {tag:04X} collides with sensor {:?}",
                        s.display_name(),
                        owner
                    ));
                } else {
                    occupied.push((tag, s.display_name()));
                }
            }
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

    /// Resolves a received `tag` against configured sensors. Returns the owning
    /// `BwWssSensor` and the inferred reading kind: `data_tag` → `Average`,
    /// `data_tag + 1` (gust enabled) → `Gust`. Search stops at the first match;
    /// [`Config::validate`] rejects colliding tags so ownership is unique.
    pub fn match_tag(&self, tag: u16) -> Option<(&BwWssSensor, ReadingKind)> {
        for sensor in &self.sensors {
            let Sensor::BwWss(s) = sensor;
            if s.data_tag == tag {
                return Some((s, ReadingKind::Average));
            }
            if s.gust && s.data_tag.checked_add(1) == Some(tag) {
                return Some((s, ReadingKind::Gust));
            }
        }
        None
    }

    /// Returns the owning sensor's display name if `tag` is already occupied by
    /// an existing sensor's base or gust slot. Used by the wizard to reject
    /// colliding tags inline. Pass `skip` = the index of the sensor being
    /// edited so its own tags don't count as a collision against itself.
    pub fn tag_owner(&self, tag: u16, skip: Option<usize>) -> Option<String> {
        for (i, sensor) in self.sensors.iter().enumerate() {
            if Some(i) == skip {
                continue;
            }
            let Sensor::BwWss(s) = sensor;
            let mut tags = vec![s.data_tag];
            if s.gust {
                if let Some(gust_tag) = s.data_tag.checked_add(1) {
                    tags.push(gust_tag);
                }
            }
            if tags.contains(&tag) {
                return Some(s.display_name().to_string());
            }
        }
        None
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
            "collides with sensor",
            "min_interval_secs must not exceed",
        ] {
            assert!(
                message.contains(expected),
                "missing {expected:?} in {message}"
            );
        }
    }

    #[test]
    fn rejects_unknown_keys() {
        let text = r#"
            [serial]
            port = "/dev/ttyUSB0"
            baude_rate = 115200
            [aggregation]
            change_percent = 20.0
            min_interval_secs = 30
            max_interval_secs = 300
        "#;
        let error = toml::from_str::<Config>(text).unwrap_err();
        assert!(error.to_string().contains("baude_rate"), "{error}");
    }

    #[test]
    fn rejects_non_finite_change_percent() {
        let mut config = example_config();
        config.aggregation.change_percent = f64::NAN;
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("finite number"));
        config.aggregation.change_percent = f64::INFINITY;
        assert!(config.validate().is_err());
    }

    #[test]
    fn gust_tag_collisions_are_rejected() {
        // Sensor A (25DF, gust) occupies 25E0; sensor B may not use it as base.
        let mut config = example_config();
        let Sensor::BwWss(mut b) = config.sensors[0].clone();
        b.name = "SECOND".into();
        b.data_tag = 0x25E0;
        b.gust = false;
        config.sensors.push(Sensor::BwWss(b));
        let message = config.validate().unwrap_err().to_string();
        assert!(message.contains("25E0 collides"), "{message}");

        // FFFF + gust leaves no room for base+1.
        let mut config = example_config();
        let Sensor::BwWss(s) = &mut config.sensors[0];
        s.data_tag = 0xFFFF;
        let message = config.validate().unwrap_err().to_string();
        assert!(message.contains("no room for the gust tag"), "{message}");
    }

    #[test]
    fn display_name_falls_back_to_id() {
        let mut s = BwWssSensor {
            name: String::new(),
            id: "1A2B3F".into(),
            unit: WindUnit::Mps,
            data_tag: 0x25DF,
            gust: false,
            average_window_secs: 10,
            gust_window_secs: 0,
            url: "https://musikfest.example".into(),
            token: "secret".into(),
        };
        assert_eq!(s.display_name(), "1A2B3F");
        s.name = "WINDMESSER1".into();
        assert_eq!(s.display_name(), "WINDMESSER1");
    }

    #[test]
    fn tag_owner_sees_base_and_gust_slots() {
        let config = example_config(); // 25DF base, gust enabled → 25E0 occupied
        assert_eq!(
            config.tag_owner(0x25DF, None).as_deref(),
            Some("WINDMESSER1")
        );
        assert_eq!(
            config.tag_owner(0x25E0, None).as_deref(),
            Some("WINDMESSER1")
        );
        assert!(config.tag_owner(0x1234, None).is_none());
    }

    #[test]
    fn tag_owner_skips_self_index() {
        let config = example_config(); // sensor 0 owns 25DF + 25E0
                                       // When editing sensor 0, its own tags must not count as collisions.
        assert_eq!(config.tag_owner(0x25DF, Some(0)), None);
        assert_eq!(config.tag_owner(0x25E0, Some(0)), None);
        // Other sensors' tags still collide.
        // (example_config has only one sensor; add a second to check cross-collar.)
        let mut config = example_config();
        let Sensor::BwWss(mut b) = config.sensors[0].clone();
        b.name = "SECOND".into();
        b.data_tag = 0x25E0;
        b.gust = false;
        config.sensors.push(Sensor::BwWss(b));
        // 25E0 is now owned by sensor 1; editing sensor 0 must see the collision.
        assert_eq!(config.tag_owner(0x25E0, Some(0)).as_deref(), Some("SECOND"));
    }

    #[test]
    fn summary_never_contains_token() {
        let summary = example_config().summary();
        assert!(summary.contains("********"), "{summary}");
        assert!(!summary.contains("secret"), "{summary}");
    }

    #[test]
    fn match_tag_resolves_base_and_gust() {
        let config = example_config();
        let (s, kind) = config.match_tag(0x25DF).unwrap();
        assert_eq!(s.display_name(), "WINDMESSER1");
        assert_eq!(kind, ReadingKind::Average);
        let (_, kind) = config.match_tag(0x25E0).unwrap();
        assert_eq!(kind, ReadingKind::Gust);
        assert!(config.match_tag(0x1234).is_none());
    }

    /// `data_tag = FFFF` with `gust = true` is rejected by `validate`, but
    /// `match_tag` must still be safe on an unvalidated `Config` (e.g. opened
    /// via `Config::read` in the wizard): the gust branch returns `None`
    /// instead of silently matching tag `0000` via wrap-around.
    #[test]
    fn match_tag_does_not_wrap_on_overflow() {
        let config = Config {
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
                name: "OVERFLOW".into(),
                id: "1A2B3F".into(),
                unit: WindUnit::Mps,
                data_tag: 0xFFFF,
                gust: true,
                average_window_secs: 10,
                gust_window_secs: 10,
                url: "https://example".into(),
                token: "secret".into(),
            })],
        };
        assert!(config.match_tag(0xFFFF).is_some()); // base still matches
        assert!(
            config.match_tag(0x0000).is_none(),
            "gust slot must not wrap to 0x0000"
        );
    }
}
