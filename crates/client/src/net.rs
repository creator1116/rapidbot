//! The network side, standing in for vanilla's Netty event loop.
//!
//! Vanilla handles a few packets directly on the network thread (their
//! handlers skip `PacketUtils.ensureRunningOnSameThread`). Everything else is
//! queued for the main thread, which drains the queue once per frame. The
//! difference is visible to the server: a keep-alive is answered at once,
//! while a ping waits for the next frame.

use std::sync::Arc;
use std::sync::mpsc as std_mpsc;

use rapidbot_buf::read_var_int;
use rapidbot_nbt::Tag;
use rapidbot_protocol::packets::{configuration, play};
use rapidbot_protocol::{Direction, Packet, RawPacket, Reader, State, Writer};
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

use crate::{clock, text};

/// When a packet was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketStage {
    /// Decoded from the socket, before anything acted on it.
    Received,
    /// Handed to the writer queue. The writer writes one packet per flush, so
    /// a queued packet may not be on the wire yet.
    Queued,
}

/// One packet seen by a [`PacketObserver`].
#[derive(Debug)]
pub struct ObservedPacket<'a> {
    /// [`ClientConfig::connection_id`](crate::ClientConfig::connection_id) of
    /// the connection it came from, so an application running several bots
    /// can tell them apart.
    pub connection: u64,
    pub direction: Direction,
    pub state: State,
    pub id: i32,
    /// The name the generated tables give this id, where they have one.
    pub name: Option<&'static str>,
    pub stage: PacketStage,
    /// The decoded (or about-to-be-queued) body without its length prefix, so
    /// the packet id is **not** in here. Borrowed for the call.
    pub body: &'a [u8],
}

/// Called for every packet an observed connection decodes or queues, on the
/// reader task or on whatever thread sent it. It must not block: an
/// application that stores or forwards packets should hand them to its own
/// bounded queue.
///
/// Bodies can hold sensitive data — login, cookies and custom payloads in
/// particular — so redacting or encrypting before persisting them is the
/// application's job.
type ObserverFn = dyn Fn(ObservedPacket<'_>) + Send + Sync;

/// An application's packet callback.
///
/// ```
/// use rapidbot_client::PacketObserver;
///
/// let observer = PacketObserver::new(|packet| {
///     let _ = (packet.id, packet.body.len(), packet.stage);
/// });
/// ```
#[derive(Clone)]
pub struct PacketObserver(Arc<ObserverFn>);

impl PacketObserver {
    /// Builds an observer from a callback.
    pub fn new<F>(observe: F) -> Self
    where
        F: Fn(ObservedPacket<'_>) + Send + Sync + 'static,
    {
        Self(Arc::new(observe))
    }

    fn call(&self, packet: ObservedPacket<'_>) {
        (self.0)(packet);
    }
}

impl std::fmt::Debug for PacketObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PacketObserver")
    }
}

/// The observer of one connection, and the id it reports it under.
#[derive(Clone, Default)]
pub struct Observer {
    connection: u64,
    callback: Option<PacketObserver>,
}

impl Observer {
    pub fn new(connection: u64, callback: Option<PacketObserver>) -> Self {
        Self {
            connection,
            callback,
        }
    }

    /// Whether anyone is listening, so a caller can skip the work of
    /// preparing a body for a callback that will not happen.
    pub fn is_observing(&self) -> bool {
        self.callback.is_some()
    }

    /// Reports an outgoing packet as it is queued.
    pub fn report_queued(&self, state: State, id: i32, body: &[u8]) {
        self.report(Direction::Serverbound, state, id, PacketStage::Queued, body);
    }

    /// Reports an incoming packet once it has been decoded.
    pub fn report_received(&self, state: State, id: i32, body: &[u8]) {
        self.report(
            Direction::Clientbound,
            state,
            id,
            PacketStage::Received,
            body,
        );
    }

    /// Reports a packet, if anyone is listening.
    fn report(&self, direction: Direction, state: State, id: i32, stage: PacketStage, body: &[u8]) {
        let Some(callback) = &self.callback else {
            return;
        };
        callback.call(ObservedPacket {
            connection: self.connection,
            direction,
            state,
            id,
            name: state.packet_name(direction, id),
            stage,
            body,
        });
    }
}

/// Serialised packets to write, in order. Senders on any thread go through
/// this one queue, like `channel.eventLoop().execute(..)`.
#[derive(Clone)]
pub struct PacketSender {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    observer: Observer,
}

impl PacketSender {
    /// A sender whose frames can be read back, for tests.
    #[cfg(test)]
    pub fn for_test() -> (PacketSender, mpsc::UnboundedReceiver<Vec<u8>>) {
        Self::for_test_with(Observer::default())
    }

    /// A test sender that also observes what it queues.
    #[cfg(test)]
    pub fn for_test_with(observer: Observer) -> (PacketSender, mpsc::UnboundedReceiver<Vec<u8>>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (PacketSender { tx, observer }, rx)
    }

    pub fn send<P: Packet>(&self, packet: &P) {
        trace!(
            id = P::ID,
            name = P::STATE.packet_name(Direction::Serverbound, P::ID),
            "send"
        );
        let frame = packet.to_frame_bytes();
        if self.observer.is_observing() {
            // The body follows the VarInt id at the front of the frame.
            let mut body = &frame[..];
            let _ = read_var_int(&mut body);
            self.observer
                .report(P::DIRECTION, P::STATE, P::ID, PacketStage::Queued, body);
        }
        // A closed channel means the connection is gone; the reader reports it.
        let _ = self.tx.send(frame);
    }
}

/// What the network side hands to the main thread.
#[derive(Debug)]
pub enum Inbound {
    Packet {
        state: State,
        packet: RawPacket,
    },
    /// A `ClientboundBundlePacket`: handled together, in order, in one go.
    Bundle(Vec<RawPacket>),
    Disconnected(String),
}

pub fn spawn_writer(
    mut writer: Writer,
    observer: Observer,
) -> (PacketSender, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let task = tokio::spawn(async move {
        while let Some(packet) = rx.recv().await {
            // Every vanilla send is writeAndFlush: one write per packet.
            if writer.queue_raw(&packet).is_err() || writer.flush().await.is_err() {
                break;
            }
        }
        let _ = writer.shutdown().await;
    });
    (PacketSender { tx, observer }, task)
}

/// `ChunkBatchSizeCalculator`: an estimate of how fast this client takes in
/// chunks, reported back so the server can pace chunk sending.
struct ChunkBatchSizeCalculator {
    aggregated_nanos_per_chunk: f64,
    old_samples_weight: i32,
    batch_start: i64,
}

impl ChunkBatchSizeCalculator {
    fn new() -> Self {
        Self {
            aggregated_nanos_per_chunk: 2_000_000.0,
            old_samples_weight: 1,
            batch_start: clock::nanos(),
        }
    }

    fn on_batch_start(&mut self) {
        self.batch_start = clock::nanos();
    }

    fn on_batch_finished(&mut self, batch_size: i32) {
        if batch_size > 0 {
            let duration = (clock::nanos() - self.batch_start) as f64;
            let per_chunk = duration / batch_size as f64;
            let clamped = crate::math::clamp_f64(
                per_chunk,
                self.aggregated_nanos_per_chunk / 3.0,
                self.aggregated_nanos_per_chunk * 3.0,
            );
            self.aggregated_nanos_per_chunk =
                (self.aggregated_nanos_per_chunk * self.old_samples_weight as f64 + clamped)
                    / (self.old_samples_weight + 1) as f64;
            self.old_samples_weight = (self.old_samples_weight + 1).min(49);
        }
    }

    fn desired_chunks_per_tick(&self) -> f32 {
        (7_000_000.0 / self.aggregated_nanos_per_chunk) as f32
    }
}

pub fn spawn_reader(
    mut reader: Reader,
    sender: PacketSender,
    inbound: std_mpsc::Sender<Inbound>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let reason = read_loop(&mut reader, &sender, &inbound).await;
        let _ = inbound.send(Inbound::Disconnected(reason));
    })
}

async fn read_loop(
    reader: &mut Reader,
    sender: &PacketSender,
    inbound: &std_mpsc::Sender<Inbound>,
) -> String {
    // The reader follows the server's view: after it sends a terminal packet
    // (finish/start configuration) the next packets belong to the new state.
    let mut state = State::Configuration;
    let mut bundle: Option<Vec<RawPacket>> = None;
    let mut chunk_batches = ChunkBatchSizeCalculator::new();

    loop {
        let packet = match reader.recv().await {
            Ok(p) => p,
            Err(e) => return e.to_string(),
        };
        trace!(
            id = packet.id,
            name = state.packet_name(Direction::Clientbound, packet.id),
            len = packet.body.len(),
            "recv"
        );
        // The state here is the one the packet belongs to: the switch to the
        // next state happens after it has been handled.
        sender.observer.report(
            Direction::Clientbound,
            state,
            packet.id,
            PacketStage::Received,
            &packet.body,
        );

        if state == State::Play {
            if packet.id == play::clientbound::BundleDelimiter::ID {
                match bundle.take() {
                    None => bundle = Some(Vec::new()),
                    Some(packets) => {
                        if inbound.send(Inbound::Bundle(packets)).is_err() {
                            return "client closed".into();
                        }
                    }
                }
                continue;
            }
            if let Some(packets) = &mut bundle {
                packets.push(packet);
                continue;
            }
        }

        match handle_on_network_thread(state, &packet, sender, &mut chunk_batches) {
            Ok(NetworkHandled::Yes) => continue,
            Ok(NetworkHandled::Disconnect(reason)) => return reason,
            Ok(NetworkHandled::No) => {}
            Err(e) => return format!("bad packet {}: {e}", packet.id),
        }

        let next_state = match (state, packet.id) {
            (State::Configuration, configuration::clientbound::FinishConfiguration::ID) => {
                Some(State::Play)
            }
            (State::Play, play::clientbound::StartConfiguration::ID) => Some(State::Configuration),
            _ => None,
        };
        if inbound.send(Inbound::Packet { state, packet }).is_err() {
            return "client closed".into();
        }
        if let Some(next) = next_state {
            debug!(?next, "reader switching state");
            state = next;
        }
    }
}

enum NetworkHandled {
    Yes,
    No,
    Disconnect(String),
}

fn handle_on_network_thread(
    state: State,
    packet: &RawPacket,
    sender: &PacketSender,
    chunk_batches: &mut ChunkBatchSizeCalculator,
) -> Result<NetworkHandled, rapidbot_buf::DecodeError> {
    use NetworkHandled::*;
    match state {
        State::Configuration => {
            use configuration::{clientbound as cb, serverbound as sb};
            match packet.id {
                cb::KeepAlive::ID => {
                    let id = packet.decode::<cb::KeepAlive>()?.id;
                    sender.send(&sb::KeepAlive { id });
                    Ok(Yes)
                }
                cb::Disconnect::ID => Ok(Disconnect(disconnect_reason(
                    &packet.decode::<cb::Disconnect>()?.reason,
                ))),
                cb::CustomPayload::ID => Ok(
                    if is_brand(&packet.decode::<cb::CustomPayload>()?.channel) {
                        No
                    } else {
                        Yes
                    },
                ),
                // Handled without the main thread; nothing to send.
                cb::UpdateEnabledFeatures::ID | cb::ResetChat::ID => Ok(No),
                _ => Ok(No),
            }
        }
        State::Play => {
            use play::{clientbound as cb, serverbound as sb};
            match packet.id {
                cb::KeepAlive::ID => {
                    let id = packet.decode::<cb::KeepAlive>()?.id;
                    sender.send(&sb::KeepAlive { id });
                    Ok(Yes)
                }
                cb::ChunkBatchStart::ID => {
                    chunk_batches.on_batch_start();
                    Ok(Yes)
                }
                cb::ChunkBatchFinished::ID => {
                    let size = packet.decode::<cb::ChunkBatchFinished>()?.batch_size;
                    chunk_batches.on_batch_finished(size);
                    sender.send(&sb::ChunkBatchReceived {
                        desired_chunks_per_tick: chunk_batches.desired_chunks_per_tick(),
                    });
                    Ok(Yes)
                }
                cb::Disconnect::ID => Ok(Disconnect(disconnect_reason(
                    &packet.decode::<cb::Disconnect>()?.reason,
                ))),
                cb::CustomPayload::ID => Ok(
                    if is_brand(&packet.decode::<cb::CustomPayload>()?.channel) {
                        No
                    } else {
                        Yes
                    },
                ),
                _ => Ok(No),
            }
        }
        other => {
            warn!(?other, "reader in unexpected state");
            Ok(No)
        }
    }
}

fn is_brand(channel: &rapidbot_buf::Identifier) -> bool {
    channel.normalized() == "minecraft:brand"
}

fn disconnect_reason(reason: &Tag) -> String {
    text::nbt_to_plain(reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Every queued packet is reported once, with the metadata an
    /// application needs, and the frame still reaches the writer queue.
    #[test]
    fn a_queued_packet_is_observed_once() {
        type Seen = (u64, State, i32, PacketStage, Vec<u8>);
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let sink = Arc::clone(&seen);
        let observer = Observer::new(
            7,
            Some(PacketObserver::new(move |packet| {
                sink.lock().unwrap().push((
                    packet.connection,
                    packet.state,
                    packet.id,
                    packet.stage,
                    packet.body.to_vec(),
                ));
            })),
        );
        let (sender, mut frames) = PacketSender::for_test_with(observer);
        let packet = play::serverbound::ClientTickEnd;
        sender.send(&packet);
        sender.send(&packet);

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "one observation per queued packet");
        assert_eq!(
            seen[0],
            (
                7,
                State::Play,
                play::serverbound::ClientTickEnd::ID,
                PacketStage::Queued,
                Vec::new(),
            )
        );
        // The body has no length prefix and no packet id in it.
        assert_eq!(
            frames.try_recv().unwrap(),
            packet.to_frame_bytes(),
            "the frame is unchanged"
        );
        assert_eq!(frames.try_recv().unwrap(), packet.to_frame_bytes());
    }

    /// A body is borrowed straight out of the frame, so a packet with
    /// contents reports exactly its encoded payload.
    #[test]
    fn an_observed_body_is_the_encoded_payload() {
        let seen: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let sink = Arc::clone(&seen);
        let observer = Observer::new(
            1,
            Some(PacketObserver::new(move |packet| {
                sink.lock().unwrap().push(packet.body.to_vec());
            })),
        );
        let (sender, _frames) = PacketSender::for_test_with(observer);
        let packet = play::serverbound::SetCarriedItem { slot: 300 };
        sender.send(&packet);

        let mut expected = Vec::new();
        rapidbot_buf::Encode::encode(&packet, &mut expected);
        assert_eq!(seen.lock().unwrap().as_slice(), [expected]);
    }

    /// Nothing is reported without an observer, and the queue still works.
    #[test]
    fn observation_is_off_by_default() {
        let (sender, mut frames) = PacketSender::for_test();
        sender.send(&play::serverbound::ClientTickEnd);
        assert!(frames.try_recv().is_ok());
    }
}
