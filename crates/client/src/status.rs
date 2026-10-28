//! Server list status ping (`ServerStatusPinger`).
//!
//! This is separate from joining: vanilla pings a server when the multiplayer
//! list is refreshed, not when connecting. The handshake still uses the
//! address as typed, while the TCP connection goes to the SRV-resolved
//! address, as `ServerStatusPinger.pingServer` does.

use std::time::Duration;

use rapidbot_protocol::packets::handshake::{Intent, Intention};
use rapidbot_protocol::packets::status::{clientbound as cb, serverbound as sb};
use rapidbot_protocol::{Connection, PROTOCOL_VERSION};

use crate::{ClientError, address, clock};

/// What a server list ping returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The JSON text of `ClientboundStatusResponsePacket`.
    pub json: String,
    /// Round trip measured around the status response and ping exchange, as
    /// vanilla reports it.
    pub latency: Duration,
}

/// Pings `address` (`host[:port]`) the way the server list does.
pub async fn ping(address_text: &str) -> Result<Status, ClientError> {
    let (host, port) = address::parse_address(address_text)?;
    let resolved = address::resolve(address_text).await?;
    let mut connection = Connection::connect(resolved.socket).await?;

    connection
        .send(&Intention {
            protocol_version: PROTOCOL_VERSION,
            // ServerStatusPinger uses the raw address, before SRV lookup.
            host: host.as_str().into(),
            port,
            intent: Intent::Status,
        })
        .await?;
    connection.send(&sb::StatusRequest).await?;

    let response = connection.recv().await?.decode::<cb::StatusResponse>()?;
    // handlePongResponse: the pong's payload is not inspected; latency is
    // the local clock around the exchange.
    let sent = clock::millis();
    connection.send(&sb::PingRequest { time: sent }).await?;
    let _pong = connection.recv().await?.decode::<cb::PongResponse>()?;
    let elapsed = clock::millis().saturating_sub(sent).max(0) as u64;

    Ok(Status {
        json: response.json,
        latency: Duration::from_millis(elapsed),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapidbot_protocol::Packet;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn ping_uses_raw_handshake_and_returns_status() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut connection = Connection::from_stream(stream).unwrap();

            let handshake = connection.recv().await.unwrap();
            assert_eq!(handshake.id, Intention::ID);
            let intention = Intention::from_body(&handshake.body).unwrap();
            assert_eq!(intention.intent, Intent::Status);
            assert_eq!(intention.host.0, "127.0.0.1");
            assert_eq!(intention.port, port);

            let request = connection.recv().await.unwrap();
            assert_eq!(request.id, sb::StatusRequest::ID);
            let json = r#"{"version":{"name":"26.3","protocol":777},"players":{"max":20,"online":0},"description":{"text":"test"}}"#;
            connection
                .writer
                .queue_raw(&cb::StatusResponse { json: json.into() }.to_frame_bytes())
                .unwrap();
            connection.writer.flush().await.unwrap();

            let ping = connection.recv().await.unwrap();
            assert_eq!(ping.id, sb::PingRequest::ID);
            let ping = sb::PingRequest::from_body(&ping.body).unwrap();
            connection
                .writer
                .queue_raw(&cb::PongResponse { time: ping.time }.to_frame_bytes())
                .unwrap();
            connection.writer.flush().await.unwrap();
        });

        let status = ping(&format!("127.0.0.1:{port}")).await.unwrap();
        assert!(status.json.contains("\"protocol\":777"));
        assert!(status.latency < Duration::from_secs(5));
        server.await.unwrap();
    }
}
