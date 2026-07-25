//! Integration test: full fixture through `run_reader`, end-to-end.

use std::io::Cursor;

use stage_safety_gateway::config::{
    AggregationConfig, BwWssSensor, Config, Sensor, SerialConfig, WindUnit,
};
use stage_safety_gateway::{run_reader, ReaderEvent, ReadingKind, FRAME_LEN};

const FIXTURE: &str = include_str!("fixtures/bw-wss-mps.hex");

fn fixture_bytes() -> Vec<u8> {
    let hex = FIXTURE.trim();
    assert!(hex.is_ascii() && hex.len().is_multiple_of(2));
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn config() -> Config {
    Config {
        serial: SerialConfig {
            port: "/dev/null".into(),
            baud_rate: 115200,
        },
        aggregation: AggregationConfig {
            change_percent: 20.0,
            min_interval_secs: 30,
            max_interval_secs: 300,
        },
        sensors: vec![Sensor::BwWss(BwWssSensor {
            name: "WINDMESSER".into(),
            id: "FD25D5".into(),
            unit: WindUnit::Mps,
            data_tag: 0x25df,
            gust: true,
            average_window_secs: 10,
            gust_window_secs: 10,
            url: "https://example".into(),
            token: "secret".into(),
        })],
    }
}

#[derive(Default, Debug)]
struct Counts {
    average: u64,
    gust: u64,
    unconfigured: u64,
    drained_total: usize,
    eof: bool,
}

/// Replayed fixture through `run_reader`: 316 frames, 158 average + 158 gust,
/// matching `tests/decoder.rs`. Lead-in `ff 00` registers a single 2-byte
/// `Drained` event before the first reading.
#[test]
fn run_reader_decodes_fixture_with_tag_split() {
    let recording = fixture_bytes();
    assert_eq!(recording.len() % FRAME_LEN, 0);

    let mut stream = vec![0xff, 0x00];
    stream.extend(recording);
    let mut cursor = Cursor::new(stream);

    let mut counts = Counts::default();
    run_reader(&mut cursor, &config(), |event| match event {
        ReaderEvent::Reading(r) => match r.kind {
            ReadingKind::WindAverage => counts.average += 1,
            ReadingKind::WindGust => counts.gust += 1,
        },
        ReaderEvent::Unconfigured(_) => counts.unconfigured += 1,
        ReaderEvent::Drained(n) => counts.drained_total += n,
        ReaderEvent::Eof => counts.eof = true,
    })
    .unwrap();

    assert_eq!(counts.average, 158);
    assert_eq!(counts.gust, 158);
    assert_eq!(counts.unconfigured, 0);
    assert_eq!(counts.drained_total, 2, "lead-in ff 00 plus no other noise");
    assert!(counts.eof);
}

/// Cutting the fixture mid-frame leaves the partial bytes in the buffer; the
/// reader emits no spurious reading and reports Eof cleanly.
#[test]
fn run_reader_truncated_stream_emits_no_spurious_reading() {
    let reading = fixture_bytes();
    let truncate = reading.len() - FRAME_LEN / 2;
    let mut cursor = Cursor::new(reading[..truncate].to_vec());

    let mut total = 0u64;
    run_reader(&mut cursor, &config(), |event| {
        if let ReaderEvent::Reading(_) = event {
            total += 1;
        }
    })
    .unwrap();

    assert_eq!(total, (truncate / FRAME_LEN) as u64);
}
