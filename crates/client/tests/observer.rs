//! `ClientConfig::packet_observer` sees both directions in every state.
//!
//! A tiny server stands in for a real one: the login handshake (which runs on
//! the un-split connection), then the configuration phase (which runs on the
//! reader and writer tasks). The run ends when the server closes the socket,
//! so the test needs no play-state login packet.

use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rapidbot_buf::{ByteArray, Identifier};
use rapidbot_client::{Account, ClientConfig, ClientError, Idle};
use rapidbot_client::{PacketObserver, PacketStage};
use rapidbot_protocol::packets::configuration::{
    clientbound as config_cb, serverbound as config_sb,
};
use rapidbot_protocol::packets::handshake::{Intent, Intention};
use rapidbot_protocol::packets::login::{
    GameProfile, clientbound as login_cb, serverbound as login_sb,
};
use rapidbot_protocol::packets::play::{clientbound as play_cb, serverbound as play_sb};
use rapidbot_protocol::{Connection, Direction, Packet, State};
use tokio::net::TcpListener;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

/// One observation, copied out of the callback so the test can look at it
/// after the run.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    connection: u64,
    direction: Direction,
    state: State,
    id: i32,
    name: Option<String>,
    stage: PacketStage,
    body: Vec<u8>,
}

async fn send<P: Packet>(connection: &mut Connection, packet: &P) -> TestResult {
    connection.writer.queue_raw(&packet.to_frame_bytes())?;
    connection.writer.flush().await?;
    Ok(())
}

async fn server(listener: TcpListener) -> TestResult {
    let (stream, _) = listener.accept().await?;
    let mut connection = Connection::from_stream(stream)?;

    let intention = connection.recv().await?;
    let intention = Intention::from_body(&intention.body)?;
    assert_eq!(intention.intent, Intent::Login);
    let hello = connection.recv().await?;
    assert_eq!(hello.id, login_sb::Hello::ID);

    send(
        &mut connection,
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

    // The client acknowledges, then brands itself and sends its information.
    for _ in 0..4 {
        let packet = tokio::time::timeout(Duration::from_secs(5), connection.recv()).await??;
        if packet.id == config_sb::ClientInformation::ID {
            break;
        }
    }
    send(&mut connection, &config_cb::FinishConfiguration).await?;
    // Wait for the client's own finish, then close: the run ends on the read
    // error.
    for _ in 0..8 {
        let packet = tokio::time::timeout(Duration::from_secs(5), connection.recv()).await;
        match packet {
            Ok(Ok(packet)) if packet.id == config_sb::FinishConfiguration::ID => break,
            Ok(Ok(_)) => continue,
            _ => break,
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_directions_are_observed_across_login_and_configuration() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_task = tokio::spawn(server(listener));

    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let mut config = ClientConfig::new(
        format!("127.0.0.1:{port}"),
        Account::Offline {
            name: "Tester".into(),
        },
    );
    config.connection_id = 7;
    config.packet_observer = Some(PacketObserver::new(move |packet| {
        sink.lock().unwrap().push(Seen {
            connection: packet.connection,
            direction: packet.direction,
            state: packet.state,
            id: packet.id,
            name: packet.name.map(str::to_string),
            stage: packet.stage,
            body: packet.body.to_vec(),
        });
    }));

    let reason = tokio::time::timeout(
        Duration::from_secs(10),
        rapidbot_client::run_with(config, Idle),
    )
    .await
    .expect("the client did not finish");
    assert!(matches!(reason, ClientError::Disconnected(_)), "{reason:?}");

    let seen = seen.lock().unwrap().clone();
    assert!(!seen.is_empty(), "nothing was observed");
    assert!(
        seen.iter().all(|packet| packet.connection == 7),
        "every observation carries the connection id"
    );

    // The login handshake is reported too, even though it runs before the
    // reader and writer tasks exist.
    let login = |direction: Direction, id: i32| {
        seen.iter().any(|packet| {
            packet.state == State::Login && packet.direction == direction && packet.id == id
        })
    };
    assert!(
        login(Direction::Serverbound, Intention::ID),
        "the intention is observed"
    );
    assert!(
        login(Direction::Serverbound, login_sb::Hello::ID),
        "hello is observed"
    );
    assert!(
        login(Direction::Clientbound, login_cb::LoginFinished::ID),
        "and so is the server's answer"
    );

    // Configuration, both ways, through the reader and writer tasks.
    let config = |direction: Direction, id: i32| {
        seen.iter().any(|packet| {
            packet.state == State::Configuration && packet.direction == direction && packet.id == id
        })
    };
    assert!(config(
        Direction::Serverbound,
        config_sb::ClientInformation::ID
    ));
    assert!(config(
        Direction::Clientbound,
        config_cb::FinishConfiguration::ID
    ));
    assert!(config(
        Direction::Serverbound,
        config_sb::FinishConfiguration::ID
    ));

    // Stages match the direction, and names come from the tables.
    for packet in &seen {
        match packet.direction {
            Direction::Clientbound => assert_eq!(packet.stage, PacketStage::Received),
            Direction::Serverbound => assert_eq!(packet.stage, PacketStage::Queued),
        }
    }
    let finish = seen
        .iter()
        .find(|packet| {
            packet.id == config_sb::FinishConfiguration::ID && packet.state == State::Configuration
        })
        .expect("the client's finish");
    assert_eq!(finish.name.as_deref(), Some("finish_configuration"));
    assert!(finish.body.is_empty(), "a unit packet has an empty body");

    // One observation per packet: the handshake is reported once each. Packet
    // ids only mean something within a state, so match on the whole key.
    let count = |state: State, direction: Direction, id: i32| {
        seen.iter()
            .filter(|p| (p.state, p.direction, p.id) == (state, direction, id))
            .count()
    };
    assert_eq!(
        count(Intention::STATE, Direction::Serverbound, Intention::ID),
        1,
        "no duplicate observations"
    );
    assert_eq!(
        count(
            login_sb::Hello::STATE,
            Direction::Serverbound,
            login_sb::Hello::ID
        ),
        1,
        "no duplicate observations"
    );

    server_task.abort();
}

/// The handshake and configuration as above, then a play packet in each
/// direction and a reconfiguration back to Configuration.
async fn server_through_play(listener: TcpListener) -> TestResult {
    let (stream, _) = listener.accept().await?;
    let mut connection = Connection::from_stream(stream)?;

    let intention = connection.recv().await?;
    let intention = Intention::from_body(&intention.body)?;
    assert_eq!(intention.intent, Intent::Login);
    let _hello = connection.recv().await?;
    send(
        &mut connection,
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

    for _ in 0..4 {
        let packet = tokio::time::timeout(Duration::from_secs(5), connection.recv()).await??;
        if packet.id == config_sb::ClientInformation::ID {
            break;
        }
    }
    send(&mut connection, &config_cb::FinishConfiguration).await?;
    for _ in 0..8 {
        match tokio::time::timeout(Duration::from_secs(5), connection.recv()).await {
            Ok(Ok(packet)) if packet.id == config_sb::FinishConfiguration::ID => break,
            Ok(Ok(_)) => continue,
            _ => break,
        }
    }

    // Play: a keep-alive, which the client answers on the network thread.
    send(&mut connection, &play_cb::KeepAlive { id: 42 }).await?;
    let mut answered = false;
    for _ in 0..8 {
        match tokio::time::timeout(Duration::from_secs(5), connection.recv()).await {
            Ok(Ok(packet)) if packet.id == play_sb::KeepAlive::ID => {
                answered = true;
                break;
            }
            Ok(Ok(_)) => continue,
            _ => break,
        }
    }
    assert!(answered, "the client answers a keep-alive");

    // Then a reconfiguration, which the client acknowledges on the play
    // connection before the reader goes back to Configuration.
    send(&mut connection, &play_cb::StartConfiguration).await?;
    let mut acknowledged = false;
    for _ in 0..8 {
        match tokio::time::timeout(Duration::from_secs(5), connection.recv()).await {
            Ok(Ok(packet)) if packet.id == play_sb::ConfigurationAcknowledged::ID => {
                acknowledged = true;
                break;
            }
            Ok(Ok(_)) => continue,
            _ => break,
        }
    }
    assert!(acknowledged, "the client acknowledges the reconfiguration");
    send(
        &mut connection,
        &config_cb::StoreCookie {
            key: Identifier::from("test:cookie"),
            payload: ByteArray(b"x".to_vec()),
        },
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn play_and_a_reconfiguration_are_observed_too() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_task = tokio::spawn(server_through_play(listener));

    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let mut config = ClientConfig::new(
        format!("127.0.0.1:{port}"),
        Account::Offline {
            name: "Tester".into(),
        },
    );
    config.connection_id = 11;
    config.packet_observer = Some(PacketObserver::new(move |packet| {
        sink.lock().unwrap().push(Seen {
            connection: packet.connection,
            direction: packet.direction,
            state: packet.state,
            id: packet.id,
            name: packet.name.map(str::to_string),
            stage: packet.stage,
            body: packet.body.to_vec(),
        });
    }));

    let reason = tokio::time::timeout(
        Duration::from_secs(15),
        rapidbot_client::run_with(config, Idle),
    )
    .await
    .expect("the client did not finish");
    assert!(matches!(reason, ClientError::Disconnected(_)), "{reason:?}");

    let seen = seen.lock().unwrap().clone();
    let count = |state: State, direction: Direction, id: i32| {
        seen.iter()
            .filter(|p| (p.state, p.direction, p.id) == (state, direction, id))
            .count()
    };
    let has = |state: State, direction: Direction, id: i32| count(state, direction, id) > 0;

    // Play, both directions.
    assert!(
        has(State::Play, Direction::Clientbound, play_cb::KeepAlive::ID),
        "the server's keep-alive"
    );
    assert!(
        has(State::Play, Direction::Serverbound, play_sb::KeepAlive::ID),
        "the client's answer, queued"
    );
    // The reconfiguration itself, and the state it moves to.
    assert!(has(
        State::Play,
        Direction::Clientbound,
        play_cb::StartConfiguration::ID
    ));
    assert_eq!(
        count(
            State::Play,
            Direction::Serverbound,
            play_sb::ConfigurationAcknowledged::ID
        ),
        1,
        "acknowledged once"
    );
    assert!(
        has(
            State::Configuration,
            Direction::Clientbound,
            config_cb::StoreCookie::ID
        ),
        "after the acknowledgement the reader is back in configuration"
    );
    assert!(
        seen.iter().all(|packet| packet.connection == 11),
        "every observation carries the connection id"
    );

    server_task.abort();
}
