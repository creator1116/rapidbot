//! Login state (`ClientHandshakePacketListenerImpl`).

use rapidbot_buf::ByteArray;
use rapidbot_protocol::Connection;
use rapidbot_protocol::packets::configuration::serverbound as config_sb;
use rapidbot_protocol::packets::login::{GameProfile, clientbound as cb, serverbound as sb};
use rapidbot_protocol::{Packet, State};
use tracing::{debug, trace};
use uuid::Uuid;

use crate::{ClientConfig, ClientError, SessionData, crypto, text};

pub(crate) async fn login(
    conn: &mut Connection,
    config: &ClientConfig,
    session: &mut SessionData,
) -> Result<(GameProfile, Uuid), ClientError> {
    conn.send(&sb::Hello { name: config.account.name().into(), profile_id: config.account.uuid() })
        .await?;

    loop {
        let packet = conn.recv().await?;
        trace!(id = packet.id, name = State::Login.packet_name(rapidbot_protocol::Direction::Clientbound, packet.id), "login packet");
        match packet.id {
            cb::Hello::ID => {
                let hello = packet.decode::<cb::Hello>()?;
                let secret = crypto::generate_secret();
                if hello.should_authenticate {
                    // authenticateServer: tell Mojang we are joining, then
                    // answer. The server verifies with hasJoined.
                    let crate::Account::Microsoft(account) = &config.account else {
                        return Err(ClientError::AuthenticationRequired);
                    };
                    let hash = crypto::server_hash(&hello.server_id.0, &secret, &hello.public_key.0);
                    rapidbot_auth::join_server(&account.access_token, account.id, &hash).await?;
                    debug!("session join accepted");
                }
                let encrypt = |data: &[u8]| {
                    crypto::rsa_encrypt(&hello.public_key.0, data)
                        .map(ByteArray)
                        .map_err(|e| ClientError::Encryption(e.to_string()))
                };
                let key = sb::Key { key: encrypt(&secret)?, encrypted_challenge: encrypt(&hello.challenge.0)? };
                // The cipher is installed once the key packet has been written.
                conn.send(&key).await?;
                conn.enable_encryption(&secret);
                debug!("encryption enabled");
            }
            cb::LoginCompression::ID => {
                let threshold = packet.decode::<cb::LoginCompression>()?.threshold;
                conn.set_compression(threshold);
                debug!(threshold, "compression enabled");
            }
            cb::CustomQuery::ID => {
                let query = packet.decode::<cb::CustomQuery>()?;
                debug!(channel = %query.channel, "custom query (answering unknown)");
                conn.send(&sb::CustomQueryAnswer { transaction_id: query.transaction_id, payload: None }).await?;
            }
            cb::CookieRequest::ID => {
                let key = packet.decode::<cb::CookieRequest>()?.key;
                let payload = session.cookies.get(&key).cloned().map(ByteArray);
                conn.send(&sb::CookieResponse { key, payload }).await?;
            }
            cb::LoginDisconnect::ID => {
                let reason = packet.decode::<cb::LoginDisconnect>()?.reason;
                return Err(ClientError::Disconnected(text::json_to_plain(&reason.0)));
            }
            cb::LoginFinished::ID => {
                let finished = packet.decode::<cb::LoginFinished>()?;
                // handleLoginFinished: acknowledge, switch to configuration,
                // then brand and client information, each flushed.
                conn.send(&sb::LoginAcknowledged).await?;
                send_brand(conn).await?;
                conn.send(&config_sb::ClientInformation(config.information.clone())).await?;
                return Ok((finished.profile, finished.session_id));
            }
            other => {
                return Err(ClientError::Protocol(format!("unexpected login packet {other}")));
            }
        }
    }
}

/// `BrandPayload(ClientBrandRetriever.getClientModName())`.
async fn send_brand(conn: &mut Connection) -> Result<(), ClientError> {
    let mut data = Vec::new();
    rapidbot_buf::Encode::encode("vanilla", &mut data);
    conn.send(&config_sb::CustomPayload {
        channel: "minecraft:brand".into(),
        data: rapidbot_buf::RemainingBytes(data),
    })
    .await?;
    Ok(())
}
