const MAX_PAYLOAD: usize = 8 * 1024 * 1024;
const MAX_BUFFERED: usize = MAX_PAYLOAD + 48;

/// Severity levels used by the trainer's `Log` pipe, matching the reference
/// client's `TrainerLogLevel` enum.
pub const LOG_VERBOSE: u32 = 0;
pub const LOG_INFO: u32 = 1;
pub const LOG_WARNING: u32 = 2;
pub const LOG_ERROR: u32 = 3;

const MAX_LOG_MESSAGE: usize = 64 * 1024;

/// One decoded message from the trainer's `Log` pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogMessage {
    pub level: u32,
    pub message: String,
}

impl LogMessage {
    pub fn is_problem(&self) -> bool {
        self.level >= LOG_WARNING
    }

    /// Win32 error code reported by the trainer, if the line carries one
    /// (`Last Win32: 0x...`). Used to surface a real cause on setup failure.
    pub fn win32_code(&self) -> Option<u32> {
        let lower = self.message.to_ascii_lowercase();
        let marker = "last win32:";
        let start = lower.find(marker)? + marker.len();
        let rest = self.message[start..].trim_start();
        let rest = rest
            .strip_prefix("0x")
            .or_else(|| rest.strip_prefix("0X"))
            .unwrap_or(rest);
        let digits: String = rest
            .chars()
            .take_while(|character| character.is_ascii_hexdigit())
            .collect();
        u32::from_str_radix(&digits, 16).ok()
    }
}

/// Decodes the trainer's `Log` pipe framing.
///
/// Layout recovered from the reference client: `[level: u32][pad: u32]
/// [byte_len: u32][pad: u32]` followed by `byte_len` bytes of UTF-16LE text.
#[derive(Default)]
pub struct LogCodec {
    pending: Vec<u8>,
}

impl LogCodec {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<LogMessage> {
        if self.pending.len() + chunk.len() > MAX_BUFFERED + MAX_LOG_MESSAGE {
            self.pending.clear();
            return Vec::new();
        }
        self.pending.extend_from_slice(chunk);
        let mut decoded = Vec::new();
        let mut offset = 0;
        while self.pending.len() - offset >= 16 {
            let bytes = &self.pending[offset..];
            let level = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
            let length = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
            if length > MAX_LOG_MESSAGE {
                self.pending.clear();
                return decoded;
            }
            if bytes.len() < 16 + length {
                break;
            }
            let body = &bytes[16..16 + length];
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            decoded.push(LogMessage {
                level,
                message: String::from_utf16_lossy(&units),
            });
            offset += 16 + length;
        }
        self.pending.drain(..offset);
        decoded
    }
}

/// Encodes one `Log` pipe message (used by tests and diagnostics tooling).
pub fn encode_log(level: u32, message: &str) -> Vec<u8> {
    let body: Vec<u8> = message
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    let mut frame = vec![0_u8; 16 + body.len()];
    frame[0..4].copy_from_slice(&level.to_le_bytes());
    frame[8..12].copy_from_slice(&(body.len() as u32).to_le_bytes());
    frame[16..].copy_from_slice(&body);
    frame
}

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Loading,
    Ready,
    Value { name: String, value: f64 },
    ValueResponse { request_id: u32, result: u32 },
    Binary { kind: String, payload: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    BufferLimit,
    PayloadLimit,
    UnknownFrameType(u32),
    InvalidField,
}

/// Decodes Wand 12.30.1's legacy trainer pipe frames. This does not attach to a game.
#[derive(Default)]
pub struct LegacyPipeCodec {
    pending: Vec<u8>,
}

impl LegacyPipeCodec {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Frame>, CodecError> {
        if chunk.len() > MAX_BUFFERED.saturating_sub(self.pending.len()) {
            return Err(CodecError::BufferLimit);
        }
        self.pending.extend_from_slice(chunk);
        let mut decoded = Vec::new();
        let mut offset = 0;
        while self.pending.len() - offset >= 8 {
            let bytes = &self.pending[offset..];
            let kind = u32::from_le_bytes(bytes[..4].try_into().unwrap());
            let length = match kind {
                0 | 1 => 8,
                2 => 56,
                3 => 24,
                4 => {
                    if bytes.len() < 48 {
                        break;
                    }
                    let payload = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
                    if payload > MAX_PAYLOAD {
                        return Err(CodecError::PayloadLimit);
                    }
                    48 + payload
                }
                other => return Err(CodecError::UnknownFrameType(other)),
            };
            if bytes.len() < length {
                break;
            }
            let frame = match kind {
                0 => Frame::Loading,
                1 => Frame::Ready,
                2 => Frame::Value {
                    name: read_ascii_field(&bytes[16..48]),
                    value: f64::from_le_bytes(bytes[48..56].try_into().unwrap()),
                },
                3 => Frame::ValueResponse {
                    request_id: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
                    result: u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
                },
                4 => Frame::Binary {
                    kind: read_ascii_field(&bytes[8..40]),
                    payload: bytes[48..length].to_vec(),
                },
                _ => unreachable!(),
            };
            decoded.push(frame);
            offset += length;
        }
        self.pending.drain(..offset);
        Ok(decoded)
    }

    pub fn encode_set_value(
        request_id: u32,
        name: &str,
        value: f64,
    ) -> Result<[u8; 56], CodecError> {
        let mut frame = [0; 56];
        frame[..4].copy_from_slice(&2_u32.to_le_bytes());
        frame[8..12].copy_from_slice(&request_id.to_le_bytes());
        write_ascii_field(&mut frame[16..48], name)?;
        frame[48..56].copy_from_slice(&value.to_le_bytes());
        Ok(frame)
    }

    pub fn encode_binary(kind: &str, payload: &[u8]) -> Result<Vec<u8>, CodecError> {
        if payload.len() > MAX_PAYLOAD {
            return Err(CodecError::PayloadLimit);
        }
        let mut frame = vec![0; 48 + payload.len()];
        frame[..4].copy_from_slice(&4_u32.to_le_bytes());
        write_ascii_field(&mut frame[8..40], kind)?;
        frame[40..44].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        frame[48..].copy_from_slice(payload);
        Ok(frame)
    }
}

fn read_ascii_field(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn write_ascii_field(destination: &mut [u8], value: &str) -> Result<(), CodecError> {
    if value.is_empty()
        || value.len() >= destination.len()
        || !value.bytes().all(|byte| byte.is_ascii() && byte != 0)
    {
        return Err(CodecError::InvalidField);
    }
    destination[..value.len()].copy_from_slice(value.as_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_partial_and_combined_frames() {
        let mut codec = LegacyPipeCodec::default();
        let mut ready = [0; 8];
        ready[..4].copy_from_slice(&1_u32.to_le_bytes());
        assert!(codec.push(&ready[..3]).unwrap().is_empty());
        assert_eq!(codec.push(&ready[3..]).unwrap(), vec![Frame::Ready]);

        let mut value = [0; 56];
        value[..4].copy_from_slice(&2_u32.to_le_bytes());
        value[16..22].copy_from_slice(b"health");
        value[48..56].copy_from_slice(&99.25_f64.to_le_bytes());
        let mut response = [0; 24];
        response[..4].copy_from_slice(&3_u32.to_le_bytes());
        response[8..12].copy_from_slice(&42_u32.to_le_bytes());
        let combined = [value.as_slice(), response.as_slice()].concat();
        assert_eq!(
            codec.push(&combined[..60]).unwrap(),
            vec![Frame::Value {
                name: "health".into(),
                value: 99.25
            }]
        );
        assert_eq!(
            codec.push(&combined[60..]).unwrap(),
            vec![Frame::ValueResponse {
                request_id: 42,
                result: 0
            }]
        );
    }

    #[test]
    fn encodes_and_decodes_binary_frame() {
        let mut codec = LegacyPipeCodec::default();
        let frame = LegacyPipeCodec::encode_binary("ping", &[1, 2, 3]).unwrap();
        assert!(codec.push(&frame[..47]).unwrap().is_empty());
        assert_eq!(
            codec.push(&frame[47..]).unwrap(),
            vec![Frame::Binary {
                kind: "ping".into(),
                payload: vec![1, 2, 3]
            }]
        );
        let set = LegacyPipeCodec::encode_set_value(42, "godmode", 1.5).unwrap();
        assert_eq!(&set[..4], &2_u32.to_le_bytes());
        assert_eq!(&set[8..12], &42_u32.to_le_bytes());
        assert_eq!(f64::from_le_bytes(set[48..56].try_into().unwrap()), 1.5);
    }

    #[test]
    fn rejects_oversized_payload() {
        let mut codec = LegacyPipeCodec::default();
        let mut header = [0; 48];
        header[..4].copy_from_slice(&4_u32.to_le_bytes());
        header[40..44].copy_from_slice(&((MAX_PAYLOAD + 1) as u32).to_le_bytes());
        assert_eq!(codec.push(&header), Err(CodecError::PayloadLimit));
    }

    #[test]
    fn decodes_fragmented_log_messages() {
        let mut codec = LogCodec::default();
        let frame = encode_log(LOG_WARNING, "Trainer setup failed. Last Win32: 0x5");
        assert!(codec.push(&frame[..7]).is_empty());
        assert!(codec.push(&frame[7..20]).is_empty());
        let decoded = codec.push(&frame[20..]);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].level, LOG_WARNING);
        assert!(decoded[0].is_problem());
        assert_eq!(decoded[0].win32_code(), Some(5));
    }

    #[test]
    fn decodes_combined_log_messages_and_info_levels() {
        let mut codec = LogCodec::default();
        let combined = [
            encode_log(LOG_INFO, "ready").as_slice(),
            encode_log(LOG_ERROR, "boom").as_slice(),
        ]
        .concat();
        let decoded = codec.push(&combined);
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].message, "ready");
        assert!(!decoded[0].is_problem());
        assert_eq!(decoded[1].message, "boom");
        assert_eq!(decoded[1].win32_code(), None);
    }

    #[test]
    fn rejects_implausible_log_lengths_without_looping() {
        let mut codec = LogCodec::default();
        let mut header = [0_u8; 16];
        header[8..12].copy_from_slice(&(u32::MAX).to_le_bytes());
        assert!(codec.push(&header).is_empty());
    }
}
