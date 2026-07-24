//! Integration test: a real 316-frame m/s recording through the public API.

use stage_safety_gateway::{next_frame, FRAME_LEN};

#[test]
fn decodes_recording_and_resynchronizes() {
    let hex = include_str!("fixtures/bw-wss-mps.hex").trim();
    assert!(hex.is_ascii() && hex.len().is_multiple_of(2));
    let recording: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
        .collect();

    let mut corrupt = recording[..FRAME_LEN].to_vec();
    corrupt[8] ^= 1;
    let mut stream = vec![0xff, 0x00];
    stream.extend(corrupt);
    stream.extend(recording);

    let mut tags = [0; 2];
    let mut count = 0;
    while let Some(frame) = next_frame(&mut stream) {
        tags[usize::from(frame.tag == 0x25e0)] += 1;
        assert!(matches!(frame.tag, 0x25df | 0x25e0));
        assert_eq!(frame.status, 0x1c);
        assert!(frame.value.is_finite() && frame.value >= 0.0);
        assert!(frame.broadcast);
        assert!(!frame.low_battery);
        assert!(!frame.error);
        count += 1;
    }

    assert_eq!(count, 316);
    assert_eq!(tags, [158, 158]);
    assert!(stream.is_empty());
}
