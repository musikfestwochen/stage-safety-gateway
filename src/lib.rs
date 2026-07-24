//! Broadweigh BW-WSS value-frame decoder.

pub mod config;

pub const FRAME_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub base_address: u8,
    pub packet_type: u8,
    pub tag: u16,
    pub status: u8,
    pub value: f32,
    pub rssi_db: i16,
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
        rssi_db: i16::from(frame[12] as i8) - 45,
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
