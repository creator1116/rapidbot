//! The network side, standing in for vanilla's Netty event loop.
//!
//! Vanilla handles a few packets directly on the network thread (their
//! handlers skip `PacketUtils.ensureRunningOnSameThread`). Everything else is
//! queued for the main thread, which drains the queue once per frame. The
//! difference is visible to the server: a keep-alive is answered at once,
//! while a ping waits for the next frame.

use std::sync::mpsc as std_mpsc;

use rapidbot_nbt::Tag;
use rapidbot_protocol::packets::{configuration, play};
use rapidbot_protocol::{Direction, Packet, RawPacket, Reader, State, Writer};
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

use crate::{clock, text};

/// Serialised packets to write, in order. Senders on any thread go through
/// this one queue, like `channel.eventLoop().execute(..)`.
#[derive(Clone)]
pub struct PacketSender {
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl PacketSender {
    /// A sender whose frames can be read back, for tests.
    #[cfg(test)]
    pub fn for_test() -> (PacketSender, mpsc::UnboundedReceiver<Vec<u8>>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (PacketSender { tx }, rx)
    }

    pub fn send<P: Packet>(&self, packet: &P) {
        trace!(
            id = P::ID,
            name = P::STATE.packet_name(Direction::Serverbound, P::ID),
            "send"
        );
        // A closed channel means the connection is gone; the reader reports it.
        let _ = self.tx.send(packet.to_frame_bytes());
    }
}

/// What the network side hands to the main thread.
#[derive(Debug)]
pub enum Inbound {
    Packet { state: State, packet: RawPacket },
    /// A `ClientboundBundlePacket`: handled together, in order, in one go.
    Bundle(Vec<RawPacket>),
    Disconnected(String),
}

pub fn spawn_writer(mut writer: Writer) -> PacketSender {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        while let Some(packet) = rx.recv().await {
            // Every vanilla send is writeAndFlush: one write per packet.
            if writer.queue_raw(&packet).is_err() || writer.flush().await.is_err() {
                break;
            }
        }
        let _ = writer.shutdown().await;
    });
    PacketSender { tx }
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
        Self { aggregated_nanos_per_chunk: 2_000_000.0, old_samples_weight: 1, batch_start: clock::nanos() }
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
            self.aggregated_nanos_per_chunk = (self.aggregated_nanos_per_chunk * self.old_samples_weight as f64
                + clamped)
                / (self.old_samples_weight + 1) as f64;
            self.old_samples_weight = (self.old_samples_weight + 1).min(49);
        }
    }

    fn desired_chunks_per_tick(&self) -> f32 {
        (7_000_000.0 / self.aggregated_nanos_per_chunk) as f32
    }
}

pub fn spawn_reader(mut reader: Reader, sender: PacketSender, inbound: std_mpsc::Sender<Inbound>) {
    tokio::spawn(async move {
        let reason = read_loop(&mut reader, &sender, &inbound).await;
        let _ = inbound.send(Inbound::Disconnected(reason));
    });
}

async fn read_loop(reader: &mut Reader, sender: &PacketSender, inbound: &std_mpsc::Sender<Inbound>) -> String {
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
            (State::Configuration, configuration::clientbound::FinishConfiguration::ID) => Some(State::Play),
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
                cb::Disconnect::ID => Ok(Disconnect(disconnect_reason(&packet.decode::<cb::Disconnect>()?.reason))),
                cb::CustomPayload::ID => Ok(if is_brand(&packet.decode::<cb::CustomPayload>()?.channel) {
                    No
                } else {
                    Yes
                }),
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
                cb::Disconnect::ID => Ok(Disconnect(disconnect_reason(&packet.decode::<cb::Disconnect>()?.reason))),
                cb::CustomPayload::ID => Ok(if is_brand(&packet.decode::<cb::CustomPayload>()?.channel) {
                    No
                } else {
                    Yes
                }),
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
