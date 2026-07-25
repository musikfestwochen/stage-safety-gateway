//! Broadweigh BW-WSS value-frame decoder.

use std::fmt;
use std::io::Read;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

pub mod config;
pub mod http;

pub const FRAME_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub base_address: u8,
    pub packet_type: u8,
    pub tag: u16,
    pub status: u8,
    pub value: f32,
    pub rssi_dbm: i16,
    pub cv: u8,
    pub broadcast: bool,
    pub low_battery: bool,
    pub error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Length,
    Header,
    Crc,
    PacketType,
    DataType,
}

pub fn crc16_modbus(data: &[u8]) -> u16 {
    let mut crc = 0xffff;
    for &byte in data {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xa001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

pub fn decode(frame: &[u8]) -> Result<Frame, DecodeError> {
    if frame.len() != FRAME_LEN {
        return Err(DecodeError::Length);
    }
    if frame[0] != 0x0b || frame[1] != 0x0b {
        return Err(DecodeError::Header);
    }
    if crc16_modbus(&frame[..14]) != u16::from_le_bytes([frame[14], frame[15]]) {
        return Err(DecodeError::Crc);
    }
    if frame[3] & 0x1f != 0x03 {
        return Err(DecodeError::PacketType);
    }
    if frame[7] & 0x07 != 0x04 {
        return Err(DecodeError::DataType);
    }

    Ok(Frame {
        base_address: frame[2],
        packet_type: frame[3],
        tag: u16::from_be_bytes([frame[4], frame[5]]),
        status: frame[6],
        value: f32::from_be_bytes([frame[8], frame[9], frame[10], frame[11]]),
        rssi_dbm: i16::from(frame[12] as i8) - 45,
        cv: frame[13] & 0x7f,
        broadcast: frame[3] & 0x20 != 0,
        low_battery: frame[3] & 0x40 != 0 || frame[6] & 0x20 != 0,
        error: frame[3] & 0x80 != 0,
    })
}

/// Removes and returns the next valid frame from an arbitrary serial byte buffer.
pub fn next_frame(buffer: &mut Vec<u8>) -> Option<Frame> {
    loop {
        let Some(start) = buffer.windows(2).position(|bytes| bytes == [0x0b, 0x0b]) else {
            if buffer.last() == Some(&0x0b) {
                let last = buffer.len() - 1;
                buffer.drain(..last);
            } else {
                buffer.clear();
            }
            return None;
        };
        buffer.drain(..start);

        if buffer.len() < FRAME_LEN {
            return None;
        }

        match decode(&buffer[..FRAME_LEN]) {
            Ok(frame) => {
                buffer.drain(..FRAME_LEN);
                return Some(frame);
            }
            Err(_) => {
                buffer.remove(0);
            }
        }
    }
}

/// Like [`next_frame`], but reports how many non-frame bytes were discarded while
/// searching since the last call (lead-in trash, invalid-frame resync steps).
/// The 16 bytes of a successfully returned frame are not counted as discarded.
pub fn next_frame_with_drain(buffer: &mut Vec<u8>) -> (Option<Frame>, usize) {
    let len_in = buffer.len();
    let frame = next_frame(buffer);
    let frame_bytes = if frame.is_some() { FRAME_LEN } else { 0 };
    let drain = len_in
        .saturating_sub(buffer.len())
        .saturating_sub(frame_bytes);
    (frame, drain)
}

/// Reading kind produced by classifying a decoded [`Frame`] against the
/// configured `data_tag` (average) and `data_tag + 1` (gust, when enabled).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingKind {
    WindAverage,
    WindGust,
}

impl fmt::Display for ReadingKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ReadingKind::WindAverage => "average",
            ReadingKind::WindGust => "gust",
        })
    }
}

/// One classified, timestamped reading ready for forwarding. `sensor` retains
/// stable ownership for routing; display names are not required to be unique.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading<'a> {
    pub sensor: &'a config::BwWssSensor,
    pub kind: ReadingKind,
    pub observed_at: DateTime<Utc>,
    pub value: f32,
    pub unit: config::WindUnit,
    pub window_seconds: u64,
    pub battery_low: bool,
    pub rssi_dbm: i16,
    pub cv: u8,
}

impl fmt::Display for Reading<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}  {}  {}  {:.4}  {}  win={}s  rssi={}dBm  cv={}  low_bat={}",
            self.observed_at
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            self.sensor.display_name(),
            self.kind,
            self.value,
            self.unit,
            self.window_seconds,
            self.rssi_dbm,
            self.cv,
            self.battery_low,
        )
    }
}

/// Constant-size send-policy state for one sensor and one [`ReadingKind`].
/// Callers own separate states for every stream.
#[derive(Debug, Default)]
pub struct PolicyState {
    previous_at: Option<DateTime<Utc>>,
    last_sent_at: Option<DateTime<Utc>>,
    last_sent_value: Option<f32>,
    last_sent_battery_low: Option<bool>,
    weighted_sum: f64,
    total_weight: f64,
    gust_min: Option<f32>,
    gust_max: Option<f32>,
    pending_change: bool,
}

impl PolicyState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one reading and returns a gateway-aggregated reading when policy
    /// requires a send. The new average value owns time since its predecessor.
    pub fn observe<'a>(
        &mut self,
        mut reading: Reading<'a>,
        policy: &config::AggregationConfig,
    ) -> Option<Reading<'a>> {
        let Some(last_sent_at) = self.last_sent_at else {
            self.record_send(&reading);
            return Some(reading);
        };

        let interval = self
            .previous_at
            .map(|previous| elapsed(reading.observed_at, previous))
            .unwrap_or_default();
        self.previous_at = Some(reading.observed_at);

        match reading.kind {
            ReadingKind::WindAverage => {
                let weight = interval.as_secs_f64();
                self.weighted_sum += f64::from(reading.value) * weight;
                self.total_weight += weight;
            }
            ReadingKind::WindGust => {
                self.gust_min = Some(
                    self.gust_min
                        .map_or(reading.value, |value| value.min(reading.value)),
                );
                self.gust_max = Some(
                    self.gust_max
                        .map_or(reading.value, |value| value.max(reading.value)),
                );
            }
        }

        let since_send = elapsed(reading.observed_at, last_sent_at);
        let last_value = self.last_sent_value.unwrap_or(reading.value);
        let threshold_value = match reading.kind {
            ReadingKind::WindAverage => self.average(reading.value),
            ReadingKind::WindGust => self.farthest_gust(last_value),
        };
        let changed = changed(last_value, threshold_value, policy.change_percent);
        let min_elapsed = since_send >= Duration::from_secs(policy.min_interval_secs);
        if changed && !min_elapsed {
            self.pending_change = true;
        }

        let battery_changed = self.last_sent_battery_low != Some(reading.battery_low);
        let value = if battery_changed {
            Some(if changed {
                threshold_value
            } else {
                self.current_aggregate(&reading)
            })
        } else if min_elapsed && (changed || self.pending_change) {
            Some(threshold_value)
        } else if since_send >= Duration::from_secs(policy.max_interval_secs) {
            Some(self.current_aggregate(&reading))
        } else {
            None
        }?;

        reading.value = value;
        reading.window_seconds = since_send.as_secs();
        self.record_send(&reading);
        Some(reading)
    }

    fn average(&self, fallback: f32) -> f32 {
        if self.total_weight > 0.0 {
            (self.weighted_sum / self.total_weight) as f32
        } else {
            fallback
        }
    }

    fn farthest_gust(&self, baseline: f32) -> f32 {
        let min = self.gust_min.unwrap_or(baseline);
        let max = self.gust_max.unwrap_or(baseline);
        if (max - baseline).abs() >= (min - baseline).abs() {
            max
        } else {
            min
        }
    }

    fn current_aggregate(&self, reading: &Reading<'_>) -> f32 {
        match reading.kind {
            ReadingKind::WindAverage => self.average(reading.value),
            ReadingKind::WindGust => self.gust_max.unwrap_or(reading.value),
        }
    }

    fn record_send(&mut self, reading: &Reading<'_>) {
        self.previous_at = Some(reading.observed_at);
        self.last_sent_at = Some(reading.observed_at);
        self.last_sent_value = Some(reading.value);
        self.last_sent_battery_low = Some(reading.battery_low);
        self.weighted_sum = 0.0;
        self.total_weight = 0.0;
        self.gust_min = None;
        self.gust_max = None;
        self.pending_change = false;
    }
}

fn elapsed(later: DateTime<Utc>, earlier: DateTime<Utc>) -> Duration {
    later
        .signed_duration_since(earlier)
        .to_std()
        .unwrap_or_default()
}

fn changed(last: f32, current: f32, percent: f64) -> bool {
    if last == 0.0 {
        current != 0.0
    } else {
        (f64::from(current) - f64::from(last)).abs() >= f64::from(last).abs() * percent / 100.0
    }
}

/// Events emitted by [`run_reader`] in stream order.
pub enum ReaderEvent<'a> {
    /// A decoded frame matched a configured sensor's tag.
    Reading(Reading<'a>),
    /// `n` non-frame bytes were discarded since the previous event. Coalesced,
    /// so this fires at most once between readings (or once per drained batch
    /// when the input ends without a final frame).
    Drained(usize),
    /// A decoded frame passed CRC/type checks but its tag matched no configured
    /// sensor. Useful on-bench signal: the listener exposes it, the daemon drops
    /// it silently.
    Unconfigured(Frame),
    /// End of input.
    Eof,
}

/// Feeds `input` into the decoder, classifies frames against `config`, and emits
/// [`ReaderEvent`]s to `on_event` in stream order. Reuses the existing
/// [`next_frame`], so resync behaviour is unchanged from the decoder tests.
///
/// For real serial ports the caller opens the port and passes a `&mut` handle;
/// for replay tests it passes a `Cursor<Vec<u8>>`. Backoff-and-reopen on unplug
/// is the daemon reader's concern; this function returns on the first `Read`
/// error so the caller decides retry policy.
pub fn run_reader<R: Read>(
    input: &mut R,
    config: &config::Config,
    mut on_event: impl FnMut(ReaderEvent<'_>),
) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let mut pending_drain = 0usize;

    loop {
        let n = input.read(&mut chunk)?;
        if n == 0 {
            if pending_drain > 0 {
                on_event(ReaderEvent::Drained(pending_drain));
            }
            on_event(ReaderEvent::Eof);
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        loop {
            let (opt, drain) = next_frame_with_drain(&mut buf);
            if drain > 0 {
                pending_drain += drain;
            }
            if let Some(frame) = opt {
                flush_drain(&mut pending_drain, &mut on_event);
                match classify(&frame, config, Utc::now()) {
                    Some(reading) => on_event(ReaderEvent::Reading(reading)),
                    None => on_event(ReaderEvent::Unconfigured(frame)),
                }
            } else {
                break;
            }
        }
    }
}

/// Emits a coalesced `Drained(n)` event if any bytes have been discarded since
/// the last non-drained event, then clears the accumulator.
fn flush_drain(pending: &mut usize, on_event: &mut impl FnMut(ReaderEvent<'_>)) {
    if *pending > 0 {
        on_event(ReaderEvent::Drained(*pending));
        *pending = 0;
    }
}

/// Stamps `frame` with `observed_at` if it matches a configured sensor's base
/// tag (→ `WindAverage`) or, when gust is enabled, `data_tag + 1` (→ `WindGust`).
/// Otherwise returns `None`. Pure: trivially unit-testable, identical mapping
/// used by the daemon reader thread later.
pub fn classify<'a>(
    frame: &Frame,
    config: &'a config::Config,
    observed_at: DateTime<Utc>,
) -> Option<Reading<'a>> {
    let (sensor, kind) = config.match_tag(frame.tag)?;
    Some(Reading {
        sensor,
        kind,
        observed_at,
        value: frame.value,
        unit: sensor.unit,
        window_seconds: match kind {
            ReadingKind::WindAverage => sensor.average_window_secs,
            ReadingKind::WindGust => sensor.gust_window_secs,
        },
        battery_low: frame.low_battery,
        rssi_dbm: frame.rssi_dbm,
        cv: frame.cv,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AggregationConfig, BwWssSensor, Config, Sensor, SerialConfig, WindUnit};

    fn sensor(name: &str, data_tag: u16, gust: bool) -> BwWssSensor {
        BwWssSensor {
            name: name.into(),
            id: "1A2B3F".into(),
            unit: WindUnit::Mps,
            data_tag,
            gust,
            average_window_secs: 10,
            gust_window_secs: 10,
            url: "https://example".into(),
            token: "secret".into(),
        }
    }

    fn config_with(sensors: Vec<BwWssSensor>) -> Config {
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
            sensors: sensors.into_iter().map(Sensor::BwWss).collect(),
        }
    }

    fn frame(tag: u16, low_battery: bool) -> Frame {
        Frame {
            base_address: 1,
            packet_type: 0x23,
            tag,
            status: if low_battery { 0x3c } else { 0x1c },
            value: 1.5,
            rssi_dbm: -50,
            cv: 100,
            broadcast: true,
            low_battery,
            error: false,
        }
    }

    fn policy(
        change_percent: f64,
        min_interval_secs: u64,
        max_interval_secs: u64,
    ) -> AggregationConfig {
        AggregationConfig {
            change_percent,
            min_interval_secs,
            max_interval_secs,
        }
    }

    fn policy_reading(
        sensor: &BwWssSensor,
        kind: ReadingKind,
        milliseconds: i64,
        value: f32,
    ) -> Reading<'_> {
        let observed_at = DateTime::parse_from_rfc3339("2026-07-25T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::milliseconds(milliseconds);
        Reading {
            sensor,
            kind,
            observed_at,
            value,
            unit: sensor.unit,
            window_seconds: match kind {
                ReadingKind::WindAverage => sensor.average_window_secs,
                ReadingKind::WindGust => sensor.gust_window_secs,
            },
            battery_low: false,
            rssi_dbm: -70,
            cv: 100,
        }
    }

    #[test]
    fn policy_sends_first_reading_then_time_weighted_heartbeat() {
        let sensor = sensor("WIND", 0x25df, true);
        let config = policy(1000.0, 0, 8);
        let mut state = PolicyState::new();

        let first = state
            .observe(
                policy_reading(&sensor, ReadingKind::WindAverage, 0, 5.0),
                &config,
            )
            .unwrap();
        assert_eq!(first.value, 5.0);
        assert_eq!(first.window_seconds, 10);
        assert!(state
            .observe(
                policy_reading(&sensor, ReadingKind::WindAverage, 2_000, 10.0),
                &config,
            )
            .is_none());

        let heartbeat = state
            .observe(
                policy_reading(&sensor, ReadingKind::WindAverage, 8_000, 20.0),
                &config,
            )
            .unwrap();
        assert!((heartbeat.value - 17.5).abs() < 0.0001);
        assert_eq!(heartbeat.window_seconds, 8);
    }

    #[test]
    fn policy_sends_at_threshold_edge_and_resets_window() {
        let sensor = sensor("WIND", 0x25df, true);
        let config = policy(20.0, 0, 300);
        let mut state = PolicyState::new();

        state
            .observe(
                policy_reading(&sensor, ReadingKind::WindAverage, 0, 10.0),
                &config,
            )
            .unwrap();
        let edge = state
            .observe(
                policy_reading(&sensor, ReadingKind::WindAverage, 10_000, 12.0),
                &config,
            )
            .unwrap();
        assert_eq!(edge.value, 12.0);
        assert_eq!(edge.window_seconds, 10);

        let next = state
            .observe(
                policy_reading(&sensor, ReadingKind::WindAverage, 15_000, 15.0),
                &config,
            )
            .unwrap();
        assert_eq!(next.value, 15.0);
        assert_eq!(next.window_seconds, 5);
    }

    #[test]
    fn policy_retains_suppressed_average_change_without_historical_candidate() {
        let sensor = sensor("WIND", 0x25df, true);
        let config = policy(20.0, 10, 300);
        let mut state = PolicyState::new();

        state
            .observe(
                policy_reading(&sensor, ReadingKind::WindAverage, 0, 10.0),
                &config,
            )
            .unwrap();
        assert!(state
            .observe(
                policy_reading(&sensor, ReadingKind::WindAverage, 5_000, 20.0),
                &config,
            )
            .is_none());

        let sent = state
            .observe(
                policy_reading(&sensor, ReadingKind::WindAverage, 10_000, 0.0),
                &config,
            )
            .unwrap();
        assert_eq!(sent.value, 10.0);
        assert_eq!(sent.window_seconds, 10);
    }

    #[test]
    fn policy_gust_sends_farthest_threshold_extreme() {
        let sensor = sensor("WIND", 0x25df, true);
        let config = policy(50.0, 10, 300);
        let mut state = PolicyState::new();

        state
            .observe(
                policy_reading(&sensor, ReadingKind::WindGust, 0, 10.0),
                &config,
            )
            .unwrap();
        for (milliseconds, value) in [(2_000, 4.0), (5_000, 14.0)] {
            assert!(state
                .observe(
                    policy_reading(&sensor, ReadingKind::WindGust, milliseconds, value),
                    &config,
                )
                .is_none());
        }

        let sent = state
            .observe(
                policy_reading(&sensor, ReadingKind::WindGust, 10_000, 11.0),
                &config,
            )
            .unwrap();
        assert_eq!(sent.value, 4.0);
        assert_eq!(sent.window_seconds, 10);
    }

    #[test]
    fn policy_gust_heartbeat_sends_maximum() {
        let sensor = sensor("WIND", 0x25df, true);
        let config = policy(1000.0, 0, 10);
        let mut state = PolicyState::new();

        state
            .observe(
                policy_reading(&sensor, ReadingKind::WindGust, 0, 10.0),
                &config,
            )
            .unwrap();
        for (milliseconds, value) in [(2_000, 8.0), (9_000, 12.0)] {
            assert!(state
                .observe(
                    policy_reading(&sensor, ReadingKind::WindGust, milliseconds, value),
                    &config,
                )
                .is_none());
        }

        let heartbeat = state
            .observe(
                policy_reading(&sensor, ReadingKind::WindGust, 10_000, 11.0),
                &config,
            )
            .unwrap();
        assert_eq!(heartbeat.value, 12.0);
    }

    #[test]
    fn policy_battery_flip_bypasses_minimum_interval() {
        let sensor = sensor("WIND", 0x25df, true);
        let config = policy(1000.0, 30, 300);
        let mut state = PolicyState::new();

        state
            .observe(
                policy_reading(&sensor, ReadingKind::WindAverage, 0, 10.0),
                &config,
            )
            .unwrap();
        let mut low = policy_reading(&sensor, ReadingKind::WindAverage, 1_000, 10.0);
        low.battery_low = true;

        let sent = state.observe(low, &config).unwrap();
        assert!(sent.battery_low);
        assert_eq!(sent.window_seconds, 1);
    }

    #[test]
    fn policy_keeps_streams_and_source_units_independent() {
        let mut sensor = sensor("WIND", 0x25df, true);
        sensor.unit = WindUnit::Kmh;
        let config = policy(20.0, 0, 300);
        let mut average = PolicyState::new();
        let mut gust = PolicyState::new();

        average
            .observe(
                policy_reading(&sensor, ReadingKind::WindAverage, 0, 0.0),
                &config,
            )
            .unwrap();
        gust.observe(
            policy_reading(&sensor, ReadingKind::WindGust, 0, 5.0),
            &config,
        )
        .unwrap();

        let sent = average
            .observe(
                policy_reading(&sensor, ReadingKind::WindAverage, 1_000, 1.0),
                &config,
            )
            .unwrap();
        assert_eq!(sent.value, 1.0);
        assert_eq!(sent.unit, WindUnit::Kmh);
        assert!(gust
            .observe(
                policy_reading(&sensor, ReadingKind::WindGust, 1_000, 5.0),
                &config,
            )
            .is_none());
    }

    #[test]
    fn policy_handles_high_frequency_window() {
        let sensor = sensor("WIND", 0x25df, true);
        let config = policy(100.0, 0, 1_800);
        let mut state = PolicyState::new();

        state
            .observe(
                policy_reading(&sensor, ReadingKind::WindAverage, 0, 1.0),
                &config,
            )
            .unwrap();
        for sample in 1..3_600 {
            assert!(state
                .observe(
                    policy_reading(&sensor, ReadingKind::WindAverage, sample * 500, 1.0,),
                    &config,
                )
                .is_none());
        }

        let heartbeat = state
            .observe(
                policy_reading(&sensor, ReadingKind::WindAverage, 1_800_000, 1.0),
                &config,
            )
            .unwrap();
        assert_eq!(heartbeat.value, 1.0);
        assert_eq!(heartbeat.window_seconds, 1_800);
    }

    #[test]
    fn classify_maps_base_tag_to_average() {
        let config = config_with(vec![sensor("WIND", 0x25df, true)]);
        let now = Utc::now();
        let reading = classify(&frame(0x25df, false), &config, now).unwrap();
        assert_eq!(reading.sensor.display_name(), "WIND");
        assert_eq!(reading.kind, ReadingKind::WindAverage);
        assert_eq!(reading.window_seconds, 10);
        assert_eq!(reading.unit, WindUnit::Mps);
        assert!(!reading.battery_low);
        assert_eq!(reading.observed_at, now);
    }

    #[test]
    fn classify_maps_gust_tag_only_when_enabled() {
        let enabled = config_with(vec![sensor("WIND", 0x25df, true)]);
        let reading = classify(&frame(0x25e0, false), &enabled, Utc::now()).unwrap();
        assert_eq!(reading.kind, ReadingKind::WindGust);
        assert_eq!(reading.window_seconds, 10);

        let disabled = config_with(vec![sensor("WIND", 0x25df, false)]);
        assert_eq!(
            classify(&frame(0x25e0, false), &disabled, Utc::now()),
            None,
            "gust tag must be dropped when gust disabled"
        );
    }

    #[test]
    fn classify_drops_unconfigured_tag() {
        let config = config_with(vec![sensor("WIND", 0x25df, true)]);
        assert_eq!(classify(&frame(0x3000, false), &config, Utc::now()), None);
    }

    #[test]
    fn classify_retains_owner_when_display_names_match() {
        let config = config_with(vec![
            sensor("WIND", 0x25df, false),
            sensor("WIND", 0x3000, false),
        ]);
        let reading = classify(&frame(0x3000, false), &config, Utc::now()).unwrap();

        assert_eq!(reading.sensor.display_name(), "WIND");
        assert_eq!(reading.sensor.data_tag, 0x3000);
    }

    #[test]
    fn classify_propagates_battery_flag() {
        let config = config_with(vec![sensor("WIND", 0x25df, true)]);
        let reading = classify(&frame(0x25df, true), &config, Utc::now()).unwrap();
        assert!(reading.battery_low);
    }

    #[test]
    fn reading_display_format_includes_millisecond_rfc3339_and_kinds() {
        let config = config_with(vec![sensor("WIND", 0x25df, true)]);
        let now = Utc::now();
        let reading = classify(&frame(0x25df, false), &config, now).unwrap();
        let line = format!("{reading}");
        assert!(
            line.starts_with(&now.to_rfc3339_opts(SecondsFormat::Millis, true)),
            "{line}"
        );
        assert!(line.contains("  WIND  "), "{line}");
        assert!(line.contains("average"), "{line}");
        assert!(line.contains("  m/s  "), "{line}");
        assert!(line.contains("win=10s"), "{line}");
        assert!(line.contains("low_bat=false"), "{line}");
    }

    #[test]
    fn reader_emits_drained_then_reading_for_resync_noise() {
        let config = config_with(vec![sensor("WIND", 0x25df, true)]);
        // Reference average frame from protocol.md, tag 25DF, CRC valid.
        let frame_bytes: [u8; 16] = [
            0x0b, 0x0b, 0x01, 0x23, 0x25, 0xdf, 0x1c, 0x04, 0x3f, 0x0f, 0x4e, 0x9f, 0xfb, 0xe8,
            0x5e, 0xa6,
        ];
        let mut stream = vec![0xff, 0x00]; // lead-in garbage
        stream.extend_from_slice(&frame_bytes);
        let mut cursor = std::io::Cursor::new(stream);

        #[derive(Debug, PartialEq)]
        enum Mk {
            Drained(usize),
            Kind(String),
            Unconfigured(u16),
            Eof,
        }
        let mut events: Vec<Mk> = Vec::new();
        run_reader(&mut cursor, &config, |event| match event {
            ReaderEvent::Reading(r) => events.push(Mk::Kind(r.kind.to_string())),
            ReaderEvent::Drained(n) => events.push(Mk::Drained(n)),
            ReaderEvent::Unconfigured(f) => events.push(Mk::Unconfigured(f.tag)),
            ReaderEvent::Eof => events.push(Mk::Eof),
        })
        .unwrap();

        assert_eq!(
            events,
            vec![Mk::Drained(2), Mk::Kind("average".into()), Mk::Eof,]
        );
    }

    #[test]
    fn reader_filters_unconfigured_valid_tag() {
        let config = config_with(vec![sensor("WIND", 0x25df, true)]);
        // Same skeleton but spoof the tag to 0x3000 — keep the CRC by recomputing.
        let mut frame_bytes: [u8; 16] = [
            0x0b, 0x0b, 0x01, 0x23, 0x30, 0x00, 0x1c, 0x04, 0x3f, 0x0f, 0x4e, 0x9f, 0xfb, 0xe8, 0,
            0,
        ];
        let crc = crc16_modbus(&frame_bytes[..14]);
        frame_bytes[14] = (crc & 0xff) as u8;
        frame_bytes[15] = (crc >> 8) as u8;
        let mut cursor = std::io::Cursor::new(frame_bytes.to_vec());

        let mut readings = 0u64;
        let mut unconfigured = 0u64;
        run_reader(&mut cursor, &config, |event| {
            if let ReaderEvent::Reading(_) = event {
                readings += 1;
            } else if let ReaderEvent::Unconfigured(_) = event {
                unconfigured += 1;
            }
        })
        .unwrap();

        assert_eq!(readings, 0);
        assert_eq!(unconfigured, 1);
    }

    /// `Drained` must fire before the next event regardless of whether that
    /// event is `Reading` or `Unconfigured`, so resync noise stays chronological.
    #[test]
    fn reader_flushes_drain_before_unconfigured_event() {
        let config = config_with(vec![sensor("WIND", 0x25df, true)]);
        let mut frame_bytes: [u8; 16] = [
            0x0b, 0x0b, 0x01, 0x23, 0x30, 0x00, 0x1c, 0x04, 0x3f, 0x0f, 0x4e, 0x9f, 0xfb, 0xe8, 0,
            0,
        ];
        let crc = crc16_modbus(&frame_bytes[..14]);
        frame_bytes[14] = (crc & 0xff) as u8;
        frame_bytes[15] = (crc >> 8) as u8;
        let mut stream = vec![0xff, 0x00]; // lead-in garbage before unconfigured frame
        stream.extend_from_slice(&frame_bytes);
        let mut cursor = std::io::Cursor::new(stream);

        #[derive(Debug, PartialEq)]
        enum Mk {
            Drained(usize),
            Unconfigured(u16),
            Eof,
        }
        let mut events: Vec<Mk> = Vec::new();
        run_reader(&mut cursor, &config, |event| match event {
            ReaderEvent::Drained(n) => events.push(Mk::Drained(n)),
            ReaderEvent::Unconfigured(f) => events.push(Mk::Unconfigured(f.tag)),
            ReaderEvent::Eof => events.push(Mk::Eof),
            ReaderEvent::Reading(_) => {}
        })
        .unwrap();

        assert_eq!(
            events,
            vec![Mk::Drained(2), Mk::Unconfigured(0x3000), Mk::Eof]
        );
    }
}
