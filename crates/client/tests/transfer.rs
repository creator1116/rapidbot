//! A server-initiated transfer reconnects and carries the cookie jar.
//!
//! Two tiny servers stand in for the old and new worlds: the first stores a
//! cookie and sends `Transfer`, the second asks for that cookie on the new
//! login. This is the only way to exercise `run_inner`'s reconnect loop
//! without a real server.

use std::error::Error;
use std::time::Duration;

use rapidbot_buf::{ByteArray, Identifier};
use rapidbot_client::{Account, ClientConfig, ClientError, Idle};
use rapidbot_nbt::Tag;
use rapidbot_protocol::packets::configuration::{
    clientbound as config_cb, serverbound as config_sb,
};
use rapidbot_protocol::packets::handshake::{Intent, Intention};
use rapidbot_protocol::packets::login::{GameProfile, clientbound as login_cb};
use rapidbot_protocol::{Connection, Packet};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

async fn send<P: Packet>(connection: &mut Connection, packet: &P) -> TestResult {
    connection.writer.queue_raw(&packet.to_frame_bytes())?;
    connection.writer.flush().await?;
    Ok(())
}

async fn login(connection: &mut Connection) -> Result<Intention, Box<dyn Error + Send + Sync>> {
    let intention = connection.recv().await?;
    let intention = Intention::from_body(&intention.body)?;
    assert_eq!(intention.intent, Intent::Login);

    let hello = connection.recv().await?;
    assert_eq!(
        hello.id,
        rapidbot_protocol::packets::login::serverbound::Hello::ID
    );

    send(
        connection,
        &login_cb::LoginFinished {
            profile: GameProfile {
                id: Uuid::from_u128(1),
                name: "Tester".into(),
                properties: Vec::new(),
            },
            session_id: Uuid::from_u128(2),
        },
    )
    .await?;
    Ok(intention)
}

async fn old_server(listener: TcpListener, new_port: u16) -> TestResult {
    let (stream, _) = listener.accept().await?;
    let mut connection = Connection::from_stream(stream)?;
    let _ = login(&mut connection).await?;

    let key: Identifier = "test:cookie".into();
    send(
        &mut connection,
        &config_cb::StoreCookie {
            key,
            payload: ByteArray(b"stored".to_vec()),
        },
    )
    .await?;
    send(
        &mut connection,
        &config_cb::Transfer {
            host: "127.0.0.1".into(),
            port: i32::from(new_port),
        },
    )
    .await?;

    // Hold the old connection until the client transfers away.
    while connection.recv().await.is_ok() {}
    Ok(())
}

async fn new_server(
    listener: TcpListener,
    port: u16,
    cookie_tx: oneshot::Sender<Option<Vec<u8>>>,
) -> TestResult {
    let (stream, _) = listener.accept().await?;
    let mut connection = Connection::from_stream(stream)?;
    let intention = login(&mut connection).await?;
    assert_eq!(intention.port, port);

    let key: Identifier = "test:cookie".into();
    send(
        &mut connection,
        &config_cb::CookieRequest { key: key.clone() },
    )
    .await?;

    let mut cookie = None;
    for _ in 0..8 {
        let packet = tokio::time::timeout(Duration::from_secs(2), connection.recv()).await??;
        if packet.id == config_sb::CookieResponse::ID {
            cookie = config_sb::CookieResponse::from_body(&packet.body)?
                .payload
                .map(|b| b.0);
            break;
        }
    }
    let _ = cookie_tx.send(cookie);

    send(
        &mut connection,
        &config_cb::Disconnect {
            reason: Tag::String("done".into()),
        },
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transfer_reconnects_and_keeps_cookies() {
    let old = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let new = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let old_port = old.local_addr().unwrap().port();
    let new_port = new.local_addr().unwrap().port();
    let (cookie_tx, cookie_rx) = oneshot::channel();

    let new_task = tokio::spawn(new_server(new, new_port, cookie_tx));
    let old_task = tokio::spawn(old_server(old, new_port));

    let config = ClientConfig::new(
        format!("127.0.0.1:{old_port}"),
        Account::Offline {
            name: "Tester".into(),
        },
    );
    let reason = tokio::time::timeout(
        Duration::from_secs(10),
        rapidbot_client::run_with(config, Idle),
    )
    .await
    .expect("client did not finish the transfer");
    assert!(matches!(reason, ClientError::Disconnected(_)), "{reason:?}");

    let cookie = tokio::time::timeout(Duration::from_secs(2), cookie_rx)
        .await
        .expect("new server did not report a cookie")
        .expect("new server task panicked");
    assert_eq!(cookie.as_deref(), Some(&b"stored"[..]));

    new_task.await.unwrap().unwrap();
    old_task.abort();
}
