use crate::tophat_codec::{GRPC_SERVICE, MAX_PAYLOAD_BYTES};
use bytes::Bytes;
use http::{Request, StatusCode, Version};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Heartbeat,
    LoadPlugin,
    ExecuteCommand,
}

impl Method {
    fn path(self) -> String {
        let name = match self {
            Self::Heartbeat => "Heartbeat",
            Self::LoadPlugin => "LoadPlugin",
            Self::ExecuteCommand => "ExecuteCommand",
        };
        format!("{GRPC_SERVICE}/{name}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcError {
    InvalidPort,
    TooLarge,
    Timeout,
    Connection(String),
    Protocol(String),
    Http(u16),
    Grpc(String),
}

pub fn frame(payload: &[u8]) -> Result<Vec<u8>, RpcError> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(RpcError::TooLarge);
    }
    let len = u32::try_from(payload.len()).map_err(|_| RpcError::TooLarge)?;
    let mut framed = Vec::with_capacity(payload.len() + 5);
    framed.push(0); // no compression
    framed.extend_from_slice(&len.to_be_bytes());
    framed.extend_from_slice(payload);
    Ok(framed)
}

pub fn unframe(bytes: &[u8]) -> Result<&[u8], RpcError> {
    if bytes.len() < 5 || bytes[0] != 0 {
        return Err(RpcError::Protocol("invalid gRPC message header".to_owned()));
    }
    let len = u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize;
    if len > MAX_PAYLOAD_BYTES || bytes.len() != len + 5 {
        return Err(RpcError::Protocol("invalid gRPC message length".to_owned()));
    }
    Ok(&bytes[5..])
}

/// Performs one raw FlatBuffers gRPC call against a loopback Tophat service.
/// A trainer session will require a persistent connection and streaming support.
pub async fn unary_local_once(
    port: u16,
    method: Method,
    payload: &[u8],
    deadline: Duration,
) -> Result<Vec<u8>, RpcError> {
    if port == 0 {
        return Err(RpcError::InvalidPort);
    }
    let framed = frame(payload)?;
    timeout(deadline, async move {
        let tcp = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|error| RpcError::Connection(error.to_string()))?;
        let (sender, connection) = h2::client::handshake(tcp)
            .await
            .map_err(|error| RpcError::Protocol(error.to_string()))?;
        let connection_task = tokio::spawn(async move { connection.await });
        let result = async move {
            let mut sender = sender
                .ready()
                .await
                .map_err(|error| RpcError::Protocol(error.to_string()))?;
            let request = Request::builder()
                .method("POST")
                .uri(format!("http://127.0.0.1:{port}{}", method.path()))
                .version(Version::HTTP_2)
                .header("content-type", "application/grpc")
                .header("te", "trailers")
                .body(())
                .map_err(|error| RpcError::Protocol(error.to_string()))?;
            let (response, mut stream) = sender
                .send_request(request, false)
                .map_err(|error| RpcError::Protocol(error.to_string()))?;
            stream
                .send_data(Bytes::from(framed), true)
                .map_err(|error| RpcError::Protocol(error.to_string()))?;
            let response = response
                .await
                .map_err(|error| RpcError::Protocol(error.to_string()))?;
            if response.status() != StatusCode::OK {
                return Err(RpcError::Http(response.status().as_u16()));
            }
            let header_status = response.headers().get("grpc-status").cloned();
            let mut body = response.into_body();
            let mut received = Vec::new();
            while let Some(chunk) = body.data().await {
                let chunk = chunk.map_err(|error| RpcError::Protocol(error.to_string()))?;
                if received.len().saturating_add(chunk.len()) > MAX_PAYLOAD_BYTES + 5 {
                    return Err(RpcError::TooLarge);
                }
                received.extend_from_slice(&chunk);
                body.flow_control()
                    .release_capacity(chunk.len())
                    .map_err(|error| RpcError::Protocol(error.to_string()))?;
            }
            let trailers = body
                .trailers()
                .await
                .map_err(|error| RpcError::Protocol(error.to_string()))?;
            let status = trailers
                .as_ref()
                .and_then(|headers| headers.get("grpc-status"))
                .or(header_status.as_ref())
                .ok_or_else(|| RpcError::Protocol("missing gRPC status".to_owned()))?;
            if status != "0" {
                return Err(RpcError::Grpc(
                    status.to_str().unwrap_or("invalid").to_owned(),
                ));
            }
            Ok(unframe(&received)?.to_vec())
        }
        .await;
        connection_task.abort();
        result
    })
    .await
    .map_err(|_| RpcError::Timeout)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::Response;

    #[test]
    fn rejects_bad_frames_and_oversized_input() {
        assert_eq!(unframe(&frame(b"hello").unwrap()).unwrap(), b"hello");
        assert!(unframe(&[1, 0, 0, 0, 0]).is_err());
        assert!(unframe(&[0, 0, 0, 0, 2, 1]).is_err());
        assert_eq!(
            frame(&vec![0; MAX_PAYLOAD_BYTES + 1]),
            Err(RpcError::TooLarge)
        );
    }

    #[test]
    fn exchanges_raw_flatbuffer_bytes_with_loopback_grpc_server() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut connection = h2::server::handshake(tcp).await.unwrap();
                let (request, mut respond) = connection.accept().await.unwrap().unwrap();
                assert_eq!(request.uri().path(), "/tophat.TophatService/ExecuteCommand");
                assert_eq!(
                    request.headers().get("content-type").unwrap(),
                    "application/grpc"
                );
                let mut body = request.into_body();
                let chunk = body.data().await.unwrap().unwrap();
                assert_eq!(unframe(&chunk).unwrap(), b"request");
                body.flow_control().release_capacity(chunk.len()).unwrap();
                let response = Response::builder()
                    .status(200)
                    .header("content-type", "application/grpc")
                    .body(())
                    .unwrap();
                let mut response_stream = respond.send_response(response, false).unwrap();
                response_stream
                    .send_data(Bytes::from(frame(b"reply").unwrap()), false)
                    .unwrap();
                let mut trailers = http::HeaderMap::new();
                trailers.insert("grpc-status", "0".parse().unwrap());
                response_stream.send_trailers(trailers).unwrap();
                while connection.accept().await.is_some() {}
            });
            let reply = unary_local_once(
                port,
                Method::ExecuteCommand,
                b"request",
                Duration::from_secs(3),
            )
            .await
            .unwrap();
            assert_eq!(reply, b"reply");
            server.await.unwrap();
        });
    }
}
