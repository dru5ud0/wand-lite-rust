use flatbuffers::FlatBufferBuilder;

pub const GRPC_SERVICE: &str = "/tophat.TophatService";
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    TooLarge,
    InvalidBuffer,
    InvalidUtf8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResponse {
    pub success: bool,
    pub error_message: Option<String>,
    pub return_value: Vec<u8>,
    pub error_code: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadPluginResponse {
    pub success: bool,
    pub error_message: Option<String>,
    pub plugin_handle: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatResponse {
    pub success: bool,
    pub server_timestamp: u64,
    pub sequence_number: u32,
    pub has_error: bool,
}

/// The installed client's HeartbeatRequest has a 64-bit timestamp and a 32-bit sequence number.
pub fn encode_heartbeat(timestamp: u64, sequence_number: u32) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let table = builder.start_table();
    builder.push_slot(4, timestamp, 0_u64);
    builder.push_slot(6, sequence_number, 0_u32);
    let table = builder.end_table(table);
    builder.finish(table, None);
    builder.finished_data().to_vec()
}

pub fn decode_heartbeat_response(bytes: &[u8]) -> Result<HeartbeatResponse, CodecError> {
    let table = TableView::new(bytes)?;
    Ok(HeartbeatResponse {
        success: table.u8(0)? != 0,
        server_timestamp: table.u64(1)?,
        sequence_number: table.u32(2)?,
        has_error: table.field(3, 4)?.is_some(),
    })
}

/// Encodes the installed client's LoadPluginRequest FlatBuffer layout.
pub fn encode_load_plugin(plugin_name: &str, plugin_path: &str) -> Result<Vec<u8>, CodecError> {
    if plugin_name.len() > 128 || plugin_path.len() > 4096 {
        return Err(CodecError::TooLarge);
    }
    let mut builder = FlatBufferBuilder::new();
    let name = builder.create_string(plugin_name);
    let path = builder.create_string(plugin_path);
    let table = builder.start_table();
    builder.push_slot_always(4, name);
    builder.push_slot_always(6, path);
    let table = builder.end_table(table);
    builder.finish(table, None);
    Ok(builder.finished_data().to_vec())
}

/// Encodes the installed client's CommandRequest layout. JSON parameters are NUL terminated.
pub fn encode_execute_json(
    command: &str,
    parameters: &serde_json::Value,
    client_id: &str,
    request_id: Option<&str>,
) -> Result<Vec<u8>, CodecError> {
    let mut bytes = serde_json::to_vec(parameters).map_err(|_| CodecError::InvalidBuffer)?;
    bytes.push(0);
    encode_execute_binary(command, Some(&bytes), client_id, request_id)
}

pub fn encode_execute_binary(
    command: &str,
    parameters: Option<&[u8]>,
    client_id: &str,
    request_id: Option<&str>,
) -> Result<Vec<u8>, CodecError> {
    if command.len() > 256
        || client_id.len() > 128
        || request_id.is_some_and(|id| id.len() > 128)
        || parameters.is_some_and(|bytes| bytes.len() > MAX_PAYLOAD_BYTES)
    {
        return Err(CodecError::TooLarge);
    }
    let mut builder = FlatBufferBuilder::new();
    let command_offset = builder.create_string(command);
    let parameters_offset = parameters.map(|bytes| builder.create_vector(bytes));
    let client_offset = builder.create_string(client_id);
    let request_offset = request_id.map(|id| builder.create_string(id));
    let table = builder.start_table();
    builder.push_slot_always(4, command_offset);
    if let Some(offset) = parameters_offset {
        builder.push_slot_always(6, offset);
    }
    builder.push_slot(8, 3_u8, 0_u8);
    builder.push_slot(10, 5000_u32, 5000_u32);
    builder.push_slot_always(12, client_offset);
    if let Some(offset) = request_offset {
        builder.push_slot_always(14, offset);
    }
    let table = builder.end_table(table);
    builder.finish(table, None);
    Ok(builder.finished_data().to_vec())
}

pub fn decode_command_response(bytes: &[u8]) -> Result<CommandResponse, CodecError> {
    let table = TableView::new(bytes)?;
    Ok(CommandResponse {
        success: table.u8(0)? != 0,
        error_message: table.string(1)?,
        return_value: table.vector(2)?,
        error_code: table.u8(6)?,
    })
}

pub fn decode_load_plugin_response(bytes: &[u8]) -> Result<LoadPluginResponse, CodecError> {
    let table = TableView::new(bytes)?;
    Ok(LoadPluginResponse {
        success: table.u8(0)? != 0,
        error_message: table.string(1)?,
        plugin_handle: table.u64(2)?,
    })
}

struct TableView<'a> {
    bytes: &'a [u8],
    table: usize,
    vtable: usize,
    vtable_size: usize,
    object_size: usize,
}

impl<'a> TableView<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, CodecError> {
        if bytes.len() > MAX_PAYLOAD_BYTES || bytes.len() < 8 {
            return Err(CodecError::InvalidBuffer);
        }
        let table = read_u32(bytes, 0)? as usize;
        let back = read_i32(bytes, table)? as isize;
        let vtable = table
            .checked_add_signed(-back)
            .ok_or(CodecError::InvalidBuffer)?;
        let vtable_size = read_u16(bytes, vtable)? as usize;
        let object_size = read_u16(bytes, vtable + 2)? as usize;
        if vtable_size < 4
            || object_size < 4
            || vtable
                .checked_add(vtable_size)
                .is_none_or(|end| end > bytes.len())
            || table
                .checked_add(object_size)
                .is_none_or(|end| end > bytes.len())
        {
            return Err(CodecError::InvalidBuffer);
        }
        Ok(Self {
            bytes,
            table,
            vtable,
            vtable_size,
            object_size,
        })
    }

    fn field(&self, index: usize, field_size: usize) -> Result<Option<usize>, CodecError> {
        let entry = 4 + index * 2;
        if entry + 2 > self.vtable_size {
            return Ok(None);
        }
        let relative = read_u16(self.bytes, self.vtable + entry)? as usize;
        if relative == 0 {
            return Ok(None);
        }
        if relative
            .checked_add(field_size)
            .is_none_or(|end| end > self.object_size)
        {
            return Err(CodecError::InvalidBuffer);
        }
        Ok(Some(self.table + relative))
    }

    fn u8(&self, index: usize) -> Result<u8, CodecError> {
        self.field(index, 1)?
            .map(|at| self.bytes[at])
            .map_or(Ok(0), Ok)
    }

    fn u64(&self, index: usize) -> Result<u64, CodecError> {
        self.field(index, 8)?
            .map(|at| read_u64(self.bytes, at))
            .unwrap_or(Ok(0))
    }

    fn u32(&self, index: usize) -> Result<u32, CodecError> {
        self.field(index, 4)?
            .map(|at| read_u32(self.bytes, at))
            .unwrap_or(Ok(0))
    }

    fn vector(&self, index: usize) -> Result<Vec<u8>, CodecError> {
        let Some(at) = self.field(index, 4)? else {
            return Ok(Vec::new());
        };
        let start = at
            .checked_add(read_u32(self.bytes, at)? as usize)
            .ok_or(CodecError::InvalidBuffer)?;
        let len = read_u32(self.bytes, start)? as usize;
        if len > MAX_PAYLOAD_BYTES {
            return Err(CodecError::TooLarge);
        }
        let body = start.checked_add(4).ok_or(CodecError::InvalidBuffer)?;
        let end = body.checked_add(len).ok_or(CodecError::InvalidBuffer)?;
        Ok(self
            .bytes
            .get(body..end)
            .ok_or(CodecError::InvalidBuffer)?
            .to_vec())
    }

    fn string(&self, index: usize) -> Result<Option<String>, CodecError> {
        let Some(at) = self.field(index, 4)? else {
            return Ok(None);
        };
        let start = at
            .checked_add(read_u32(self.bytes, at)? as usize)
            .ok_or(CodecError::InvalidBuffer)?;
        let len = read_u32(self.bytes, start)? as usize;
        if len > MAX_PAYLOAD_BYTES {
            return Err(CodecError::TooLarge);
        }
        let body = start.checked_add(4).ok_or(CodecError::InvalidBuffer)?;
        let end = body.checked_add(len).ok_or(CodecError::InvalidBuffer)?;
        let string =
            std::str::from_utf8(self.bytes.get(body..end).ok_or(CodecError::InvalidBuffer)?)
                .map_err(|_| CodecError::InvalidUtf8)?;
        Ok(Some(string.to_owned()))
    }
}

fn read_u16(bytes: &[u8], at: usize) -> Result<u16, CodecError> {
    let end = at.checked_add(2).ok_or(CodecError::InvalidBuffer)?;
    Ok(u16::from_le_bytes(
        bytes
            .get(at..end)
            .ok_or(CodecError::InvalidBuffer)?
            .try_into()
            .unwrap(),
    ))
}
fn read_u32(bytes: &[u8], at: usize) -> Result<u32, CodecError> {
    let end = at.checked_add(4).ok_or(CodecError::InvalidBuffer)?;
    Ok(u32::from_le_bytes(
        bytes
            .get(at..end)
            .ok_or(CodecError::InvalidBuffer)?
            .try_into()
            .unwrap(),
    ))
}
fn read_i32(bytes: &[u8], at: usize) -> Result<i32, CodecError> {
    let end = at.checked_add(4).ok_or(CodecError::InvalidBuffer)?;
    Ok(i32::from_le_bytes(
        bytes
            .get(at..end)
            .ok_or(CodecError::InvalidBuffer)?
            .try_into()
            .unwrap(),
    ))
}
fn read_u64(bytes: &[u8], at: usize) -> Result<u64, CodecError> {
    let end = at.checked_add(8).ok_or(CodecError::InvalidBuffer)?;
    Ok(u64::from_le_bytes(
        bytes
            .get(at..end)
            .ok_or(CodecError::InvalidBuffer)?
            .try_into()
            .unwrap(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_fields_match_the_recovered_flatbuffer_layout() {
        let request = encode_heartbeat(1_794_596_257_340, 27);
        let table = TableView::new(&request).unwrap();
        assert_eq!(table.u64(0).unwrap(), 1_794_596_257_340);
        assert_eq!(table.u32(1).unwrap(), 27);

        let mut builder = FlatBufferBuilder::new();
        let response = builder.start_table();
        builder.push_slot(4, 1_u8, 0_u8);
        builder.push_slot(6, 123_u64, 0_u64);
        builder.push_slot(8, 27_u32, 0_u32);
        let response = builder.end_table(response);
        builder.finish(response, None);
        assert_eq!(
            decode_heartbeat_response(builder.finished_data()).unwrap(),
            HeartbeatResponse {
                success: true,
                server_timestamp: 123,
                sequence_number: 27,
                has_error: false,
            }
        );
    }

    #[test]
    fn encodes_client_command_fields_and_nul_terminated_json() {
        let bytes = encode_execute_json(
            "trainer_run_mod_json",
            &serde_json::json!({"pid": 42}),
            "client",
            None,
        )
        .unwrap();
        let table = TableView::new(&bytes).unwrap();
        assert_eq!(
            table.string(0).unwrap().as_deref(),
            Some("trainer_run_mod_json")
        );
        assert_eq!(table.vector(1).unwrap(), b"{\"pid\":42}\0");
        assert_eq!(table.u8(2).unwrap(), 3);
        assert_eq!(table.string(4).unwrap().as_deref(), Some("client"));
    }

    #[test]
    fn decodes_replies_and_rejects_truncated_payloads() {
        let mut builder = FlatBufferBuilder::new();
        let error = builder.create_string("bad plugin");
        let table = builder.start_table();
        builder.push_slot(4, 0_u8, 0_u8);
        builder.push_slot_always(6, error);
        builder.push_slot(8, 99_u64, 0_u64);
        let table = builder.end_table(table);
        builder.finish(table, None);
        let bytes = builder.finished_data();
        let reply = decode_load_plugin_response(bytes).unwrap();
        assert_eq!(reply.error_message.as_deref(), Some("bad plugin"));
        assert_eq!(reply.plugin_handle, 99);
        let error_field = TableView::new(bytes).unwrap().field(1, 4).unwrap().unwrap();
        let error_start = error_field + read_u32(bytes, error_field).unwrap() as usize;
        let error_end = error_start + 4 + read_u32(bytes, error_start).unwrap() as usize;
        assert!(decode_load_plugin_response(&bytes[..error_end - 1]).is_err());
    }

    #[test]
    fn limits_input_size() {
        assert_eq!(
            encode_execute_binary("x", Some(&vec![0; MAX_PAYLOAD_BYTES + 1]), "client", None),
            Err(CodecError::TooLarge)
        );
    }
}
