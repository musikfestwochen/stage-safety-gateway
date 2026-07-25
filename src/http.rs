//! Blocking Musikfestapp ingestion transport.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use chrono::SecondsFormat;
use log::debug;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, RETRY_AFTER};
use serde::{Deserialize, Serialize};

use crate::config::BwWssSensor;
use crate::{Reading, ReadingKind};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IngestionRequest {
    pub schema_version: u8,
    pub sensor_identifier: String,
    pub observed_at: String,
    pub payload: IngestionPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IngestionPayload {
    pub kind: ReadingKind,
    pub value: f32,
    pub unit: &'static str,
    pub window_seconds: u64,
    pub battery_low: bool,
    pub rssi_dbm: i16,
    pub cv: u8,
}

impl From<&Reading<'_>> for IngestionRequest {
    fn from(reading: &Reading<'_>) -> Self {
        Self {
            schema_version: 1,
            sensor_identifier: reading.sensor.id.to_ascii_uppercase(),
            observed_at: reading
                .observed_at
                .to_rfc3339_opts(SecondsFormat::Secs, true),
            payload: IngestionPayload {
                kind: reading.kind,
                value: reading.sensor.unit.to_mps(reading.value),
                unit: "m/s",
                window_seconds: reading.window_seconds,
                battery_low: reading.battery_low,
                rssi_dbm: reading.rssi_dbm,
                cv: reading.cv,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryReason {
    Network(String),
    RateLimited,
    Server(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermanentFailure {
    Unauthorized,
    Forbidden,
    InvalidPayload,
    SensorIdentifier,
    InvalidRequest(String),
    HttpStatus(u16),
}

impl fmt::Display for PermanentFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("API token is missing, invalid, expired, or revoked"),
            Self::Forbidden => f.write_str("token is not authorized for this active sensor"),
            Self::InvalidPayload => f.write_str("gateway produced an invalid ingestion payload"),
            Self::SensorIdentifier => {
                f.write_str("configured sensor identifier does not match the token-bound sensor")
            }
            Self::InvalidRequest(error) => write!(f, "cannot construct ingestion request: {error}"),
            Self::HttpStatus(status) => write!(f, "ingestion API returned HTTP {status}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportOutcome {
    Delivered,
    Retry {
        reason: RetryReason,
        retry_after: Option<Duration>,
    },
    Permanent(PermanentFailure),
}

pub struct IngestionClient {
    client: Client,
    url: String,
    log_url: String,
    token: String,
}

#[derive(Deserialize)]
struct ValidationErrorResponse {
    #[serde(default)]
    errors: HashMap<String, Vec<String>>,
}

impl fmt::Debug for IngestionClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IngestionClient")
            .field("url", &self.log_url)
            .field("token", &"********")
            .finish_non_exhaustive()
    }
}

impl IngestionClient {
    pub fn new(sensor: &BwWssSensor) -> Result<Self, reqwest::Error> {
        Self::with_timeout(sensor, REQUEST_TIMEOUT)
    }

    fn with_timeout(sensor: &BwWssSensor, timeout: Duration) -> Result<Self, reqwest::Error> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()?;
        let request = client.post(&sensor.url).build()?;
        let mut log_url = request.url().clone();
        log_url
            .set_password(None)
            .expect("HTTP URLs support password removal");
        log_url
            .set_username("")
            .expect("HTTP URLs support username removal");
        log_url.set_query(None);
        log_url.set_fragment(None);
        Ok(Self {
            client,
            url: sensor.url.clone(),
            log_url: log_url.to_string(),
            token: sensor.token.clone(),
        })
    }

    /// Performs one HTTP attempt. Retry timing and queue ownership belong to the daemon.
    pub fn send(&self, request: &IngestionRequest) -> TransportOutcome {
        debug!("POST {} payload={request:?}", self.log_url);
        let response = match self
            .client
            .post(&self.url)
            .header(ACCEPT, "application/json")
            .bearer_auth(&self.token)
            .json(request)
            .send()
        {
            Ok(response) => response,
            Err(error) if error.is_builder() || error.is_body() => {
                return TransportOutcome::Permanent(PermanentFailure::InvalidRequest(
                    error.to_string(),
                ));
            }
            Err(error) => {
                return TransportOutcome::Retry {
                    reason: RetryReason::Network(error.to_string()),
                    retry_after: None,
                };
            }
        };

        let status = response.status();
        debug!("POST {} returned HTTP {status}", self.log_url);
        if status.is_success() {
            return TransportOutcome::Delivered;
        }
        if status.as_u16() == 429 {
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok())
                .map(Duration::from_secs);
            return TransportOutcome::Retry {
                reason: RetryReason::RateLimited,
                retry_after,
            };
        }
        if status.as_u16() == 408 || status.is_server_error() {
            return TransportOutcome::Retry {
                reason: RetryReason::Server(status.as_u16()),
                retry_after: None,
            };
        }

        TransportOutcome::Permanent(match status.as_u16() {
            401 => PermanentFailure::Unauthorized,
            403 => PermanentFailure::Forbidden,
            422 => match response.json::<ValidationErrorResponse>() {
                Ok(body) if body.errors.contains_key("sensor_identifier") => {
                    PermanentFailure::SensorIdentifier
                }
                _ => PermanentFailure::InvalidPayload,
            },
            status => PermanentFailure::HttpStatus(status),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WindUnit;
    use chrono::{DateTime, Utc};
    use httpmock::prelude::*;
    use serde_json::json;

    fn sensor(url: &str, unit: WindUnit) -> BwWssSensor {
        BwWssSensor {
            name: "WIND".into(),
            id: "1a2b3f".into(),
            unit,
            data_tag: 0x25df,
            gust: true,
            average_window_secs: 10,
            gust_window_secs: 3,
            url: url.into(),
            token: "top-secret".into(),
        }
    }

    fn reading(sensor: &BwWssSensor, value: f32) -> Reading<'_> {
        Reading {
            sensor,
            kind: ReadingKind::WindGust,
            observed_at: DateTime::parse_from_rfc3339("2026-07-23T12:00:00.123Z")
                .unwrap()
                .with_timezone(&Utc),
            value,
            unit: sensor.unit,
            window_seconds: sensor.gust_window_secs,
            battery_low: false,
            rssi_dbm: -70,
            cv: 103,
        }
    }

    #[test]
    fn posts_exact_contract_with_secret_headers() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/stage-safety/readings")
                .header("authorization", "Bearer top-secret")
                .header("accept", "application/json")
                .header("content-type", "application/json")
                .json_body(json!({
                    "schema_version": 1,
                    "sensor_identifier": "1A2B3F",
                    "observed_at": "2026-07-23T12:00:00Z",
                    "payload": {
                        "kind": "wind_gust",
                        "value": 10.0,
                        "unit": "m/s",
                        "window_seconds": 3,
                        "battery_low": false,
                        "rssi_dbm": -70,
                        "cv": 103
                    }
                }));
            then.status(200);
        });
        let sensor = sensor(&server.url("/stage-safety/readings"), WindUnit::Kmh);
        let request = IngestionRequest::from(&reading(&sensor, 36.0));
        let client = IngestionClient::new(&sensor).unwrap();

        assert_eq!(client.send(&request), TransportOutcome::Delivered);
        mock.assert_calls(1);
    }

    #[test]
    fn converts_every_source_unit_once() {
        for (unit, source, expected) in [
            (WindUnit::Mps, 10.0, 10.0),
            (WindUnit::Kmh, 36.0, 10.0),
            (WindUnit::Mph, 10.0, 4.4704),
            (WindUnit::Fps, 10.0, 3.048),
            (WindUnit::Kn, 10.0, 5.14444),
        ] {
            let sensor = sensor("https://example.test", unit);
            let request = IngestionRequest::from(&reading(&sensor, source));
            assert!((request.payload.value - expected).abs() < 0.00001);
            assert_eq!(request.payload.unit, "m/s");
            assert_eq!(request.payload.window_seconds, 3);

            let retry_copy = request.clone();
            assert_eq!(retry_copy.payload.value, request.payload.value);
        }
    }

    #[test]
    fn request_uses_owning_sensor_unit_and_aggregated_window() {
        let sensor = sensor("https://example.test", WindUnit::Kmh);
        let mut reading = reading(&sensor, 36.0);
        reading.kind = ReadingKind::WindAverage;
        reading.unit = WindUnit::Mph;
        reading.window_seconds = 999;

        let request = IngestionRequest::from(&reading);

        assert_eq!(request.payload.value, 10.0);
        assert_eq!(request.payload.window_seconds, 999);
        assert_eq!(request.sensor_identifier, "1A2B3F");
        assert_eq!(request.observed_at, "2026-07-23T12:00:00Z");
        assert_eq!(
            serde_json::to_value(&request).unwrap()["payload"]["kind"],
            "wind_average"
        );
    }

    #[test]
    fn classifies_retryable_and_permanent_responses() {
        for (status, retry_after, expected) in [
            (
                408,
                None,
                TransportOutcome::Retry {
                    reason: RetryReason::Server(408),
                    retry_after: None,
                },
            ),
            (
                429,
                Some("7"),
                TransportOutcome::Retry {
                    reason: RetryReason::RateLimited,
                    retry_after: Some(Duration::from_secs(7)),
                },
            ),
            (
                500,
                None,
                TransportOutcome::Retry {
                    reason: RetryReason::Server(500),
                    retry_after: None,
                },
            ),
            (
                401,
                None,
                TransportOutcome::Permanent(PermanentFailure::Unauthorized),
            ),
            (
                403,
                None,
                TransportOutcome::Permanent(PermanentFailure::Forbidden),
            ),
            (
                422,
                None,
                TransportOutcome::Permanent(PermanentFailure::InvalidPayload),
            ),
        ] {
            let server = MockServer::start();
            let mock = server.mock(|when, then| {
                when.method(POST).path("/readings");
                let then = then.status(status);
                if let Some(value) = retry_after {
                    then.header("Retry-After", value);
                }
            });
            let sensor = sensor(&server.url("/readings"), WindUnit::Mps);
            let request = IngestionRequest::from(&reading(&sensor, 1.0));
            assert_eq!(
                IngestionClient::new(&sensor).unwrap().send(&request),
                expected
            );
            mock.assert_calls(1);
        }
    }

    #[test]
    fn network_errors_are_retryable_and_secrets_are_redacted() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/readings", listener.local_addr().unwrap());
        drop(listener);
        let sensor = sensor(&url, WindUnit::Mps);
        let client = IngestionClient::new(&sensor).unwrap();
        let debug = format!("{client:?}");

        assert!(matches!(
            client.send(&IngestionRequest::from(&reading(&sensor, 1.0))),
            TransportOutcome::Retry {
                reason: RetryReason::Network(_),
                retry_after: None
            }
        ));
        assert!(debug.contains("********"));
        assert!(!debug.contains("top-secret"));
    }

    #[test]
    fn stalled_response_times_out_as_retryable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/readings", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(100));
        });
        let sensor = sensor(&url, WindUnit::Mps);
        let client = IngestionClient::with_timeout(&sensor, Duration::from_millis(10)).unwrap();

        assert!(matches!(
            client.send(&IngestionRequest::from(&reading(&sensor, 1.0))),
            TransportOutcome::Retry {
                reason: RetryReason::Network(_),
                retry_after: None
            }
        ));
        server.join().unwrap();
    }

    #[test]
    fn identifier_validation_error_is_sensor_configuration_failure() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/readings");
            then.status(422).json_body(json!({
                "message": "The sensor identifier field is invalid.",
                "errors": {
                    "sensor_identifier": ["The sensor identifier does not match."]
                }
            }));
        });
        let sensor = sensor(&server.url("/readings"), WindUnit::Mps);

        assert_eq!(
            IngestionClient::new(&sensor)
                .unwrap()
                .send(&IngestionRequest::from(&reading(&sensor, 1.0))),
            TransportOutcome::Permanent(PermanentFailure::SensorIdentifier)
        );
        mock.assert_calls(1);
    }

    #[test]
    fn malformed_url_is_rejected_at_startup_without_exposing_token() {
        let sensor = sensor("http://[invalid", WindUnit::Mps);
        let error = IngestionClient::new(&sensor).unwrap_err();
        let formatted = error.to_string();

        assert!(!formatted.contains("top-secret"));
    }

    #[test]
    fn debug_url_removes_credentials_query_and_fragment() {
        let sensor = sensor(
            "https://user:password@example.test/readings?api_token=secret#fragment",
            WindUnit::Mps,
        );
        let client = IngestionClient::new(&sensor).unwrap();
        let debug = format!("{client:?}");

        assert!(debug.contains("https://example.test/readings"));
        for secret in ["user", "password", "api_token", "secret", "fragment"] {
            assert!(!debug.contains(secret));
        }
    }
}
