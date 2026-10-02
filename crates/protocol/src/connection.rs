//! Async TCP connection with vanilla's socket behaviour.

use std::io;
use std::time::Duration;

use rapidbot_buf::{Decode, DecodeError, VarInt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpStream, ToSocketAddrs};

use crate::frame::{FrameDecoder, FrameEncoder, FrameError};
use crate::{Direction, Packet};

/// Vanilla's `ReadTimeoutHandler(30)`.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("connection closed by server")]
    Closed,
    #[error("timed out")]
    TimedOut,
}

/// A packet whose body has not been decoded yet.
#[derive(Debug, Clone)]
pub struct RawPacket {
    pub id: i32,
    pub body: Vec<u8>,
}

impl RawPacket {
    pub fn is<P: Packet>(&self) -> bool {
        self.id == P::ID
    }

    pub fn decode<P: Packet>(&self) -> Result<P, DecodeError> {
        debug_assert_eq!(P::DIRECTION, Direction::Clientbound);
        if self.id != P::ID {
            return Err(DecodeError::Custom(format!("expected packet {}, got {}", P::ID, self.id)));
        }
        P::from_body(&self.body)
    }
}

pub struct Connection {
    pub reader: Reader,
    pub writer: Writer,
}

impl Connection {
    pub async fn connect(addr: impl ToSocketAddrs) -> io::Result<Self> {
        Self::from_stream(TcpStream::connect(addr).await?)
    }

    pub fn from_stream(stream: TcpStream) -> io::Result<Self> {
        // Vanilla sets TCP_NODELAY on every client connection.
        stream.set_nodelay(true)?;
        let (read, write) = stream.into_split();
        Ok(Self {
            reader: Reader { stream: read, decoder: FrameDecoder::default(), read_buf: vec![0; 64 * 1024] },
            writer: Writer { stream: write, encoder: FrameEncoder::default(), pending: Vec::new() },
        })
    }

    pub async fn send<P: Packet>(&mut self, packet: &P) -> Result<(), ConnectionError> {
        self.writer.send(packet).await
    }

    pub async fn recv(&mut self) -> Result<RawPacket, ConnectionError> {
        self.reader.recv().await
    }

    pub fn set_compression(&mut self, threshold: i32) {
        self.reader.decoder.set_compression(threshold);
        self.writer.encoder.set_compression(threshold);
    }

    /// Call right after sending the encryption response: vanilla enables the
    /// cipher on both directions at that point.
    pub fn enable_encryption(&mut self, secret: &[u8; 16]) {
        self.reader.decoder.enable_encryption(secret);
        self.writer.encoder.enable_encryption(secret);
    }

    pub fn into_split(self) -> (Reader, Writer) {
        (self.reader, self.writer)
    }
}

pub struct Reader {
    stream: OwnedReadHalf,
    decoder: FrameDecoder,
    read_buf: Vec<u8>,
}

impl Reader {
    pub async fn recv(&mut self) -> Result<RawPacket, ConnectionError> {
        loop {
            if let Some(frame) = self.decoder.next_packet()? {
                let mut body = frame.as_slice();
                let id = VarInt::decode(&mut body)?.0;
                let consumed = frame.len() - body.len();
                let mut frame = frame;
                frame.drain(..consumed);
                return Ok(RawPacket { id, body: frame });
            }
            let n = tokio::time::timeout(READ_TIMEOUT, self.stream.read(&mut self.read_buf))
                .await
                .map_err(|_| ConnectionError::TimedOut)??;
            if n == 0 {
                return Err(ConnectionError::Closed);
            }
            self.decoder.feed(&self.read_buf[..n]);
        }
    }

    pub fn set_compression(&mut self, threshold: i32) {
        self.decoder.set_compression(threshold);
    }
}

pub struct Writer {
    stream: OwnedWriteHalf,
    encoder: FrameEncoder,
    /// Framed bytes queued but not yet written, like a Netty `write` without
    /// `flush`.
    pending: Vec<u8>,
}

impl Writer {
    /// Frames a packet without sending it. Queued packets go out together on
    /// the next [`flush`](Self::flush), in one TCP write.
    pub fn queue<P: Packet>(&mut self, packet: &P) -> Result<(), ConnectionError> {
        debug_assert_eq!(P::DIRECTION, Direction::Serverbound);
        self.encoder.encode(&packet.to_frame_bytes(), &mut self.pending)?;
        Ok(())
    }

    pub async fn flush(&mut self) -> Result<(), ConnectionError> {
        if !self.pending.is_empty() {
            self.stream.write_all(&self.pending).await?;
            self.pending.clear();
        }
        Ok(())
    }

    pub async fn send<P: Packet>(&mut self, packet: &P) -> Result<(), ConnectionError> {
        self.queue(packet)?;
        self.flush().await
    }

    /// Like [`queue`](Self::queue) for a packet already serialised with
    /// [`Packet::to_frame_bytes`].
    pub fn queue_raw(&mut self, packet: &[u8]) -> Result<(), ConnectionError> {
        self.encoder.encode(packet, &mut self.pending)?;
        Ok(())
    }

    pub fn set_compression(&mut self, threshold: i32) {
        self.encoder.set_compression(threshold);
    }

    pub async fn shutdown(&mut self) -> Result<(), ConnectionError> {
        self.stream.shutdown().await?;
        Ok(())
    }
}
