//! Server list ping, sent the way the vanilla client sends it.
//!
//!     cargo run -p rapidbot-protocol --example ping -- play.example.net 25565

use std::time::Instant;

use rapidbot_protocol::packets::handshake::{Intent, Intention};
use rapidbot_protocol::packets::status::{clientbound, serverbound};
use rapidbot_protocol::{Connection, PROTOCOL_VERSION};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "localhost".into());
    let port: u16 = args.next().map(|p| p.parse()).transpose()?.unwrap_or(25565);

    let mut conn = Connection::connect((host.as_str(), port)).await?;

    // For status pings vanilla sends the address as typed, before SRV lookup.
    conn.writer.queue(&Intention {
        protocol_version: PROTOCOL_VERSION,
        host: host.as_str().into(),
        port,
        intent: Intent::Status,
    })?;
    conn.send(&serverbound::StatusRequest).await?;

    let status = conn.recv().await?.decode::<clientbound::StatusResponse>()?;
    println!("{}", status.json);

    // Stand-in for System.nanoTime() / 1e6; the real client clock lives in
    // the client crate.
    let start = Instant::now();
    let time = 1_000_000 + start.elapsed().as_millis() as i64;
    conn.send(&serverbound::PingRequest { time }).await?;
    let pong = conn.recv().await?.decode::<clientbound::PongResponse>()?;
    assert_eq!(pong.time, time);
    println!("ping: {} ms", start.elapsed().as_millis());
    Ok(())
}
