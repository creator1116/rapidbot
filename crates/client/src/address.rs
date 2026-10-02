//! Server address resolution, reproducing what vanilla puts in the handshake.
//!
//! `ConnectScreen` → `ServerNameResolver.resolveAddress`:
//! 1. `ServerAddress.parseString`: `host[:port]`, default port 25565.
//! 2. SRV lookup of `_minecraft._tcp.<host>`, **only when the port is 25565**,
//!    taking the first record JNDI returns (no priority/weight sorting). The
//!    target keeps DNS's trailing dot.
//! 3. `InetAddress.getByName(host)` for the (possibly redirected) host, via
//!    the OS resolver.
//! 4. The handshake carries `InetAddress.getHostName()`: the name passed to
//!    `getByName`, or for an IP literal a forward-confirmed reverse lookup.

use std::net::{IpAddr, SocketAddr};

use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RData;
use tracing::debug;

pub const DEFAULT_PORT: u16 = 25565;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAddress {
    /// Where to open the TCP connection.
    pub socket: SocketAddr,
    /// Host and port for the handshake packet.
    pub handshake_host: String,
    pub handshake_port: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("invalid server address {0:?}")]
    Invalid(String),
    #[error("unknown host {0}")]
    UnknownHost(String),
    #[error("DNS resolver: {0}")]
    Resolver(String),
}

/// Guava `HostAndPort.fromString(..).withDefaultPort(25565)`.
pub fn parse_address(input: &str) -> Result<(String, u16), ResolveError> {
    let invalid = || ResolveError::Invalid(input.to_owned());
    let input = input.trim();
    let (host, port) = if let Some(rest) = input.strip_prefix('[') {
        // [ipv6]:port
        let (host, after) = rest.split_once(']').ok_or_else(invalid)?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p),
            None if after.is_empty() => None,
            None => return Err(invalid()),
        };
        (host, port)
    } else if input.matches(':').count() == 1 {
        let (host, port) = input.split_once(':').unwrap();
        (host, Some(port))
    } else {
        // No colon, or a bare IPv6 literal.
        (input, None)
    };
    if host.is_empty() {
        return Err(invalid());
    }
    let port = match port {
        Some(p) => p.parse().map_err(|_| invalid())?,
        None => DEFAULT_PORT,
    };
    Ok((host.to_owned(), port))
}

pub async fn resolve(input: &str) -> Result<ResolvedAddress, ResolveError> {
    let (mut host, mut port) = parse_address(input)?;
    let resolver = TokioResolver::builder_tokio()
        .map_err(|e| ResolveError::Resolver(e.to_string()))?
        .build();

    if port == DEFAULT_PORT && host.parse::<IpAddr>().is_err() {
        let name = format!("_minecraft._tcp.{host}");
        if let Ok(lookup) = resolver.srv_lookup(name.as_str()).await {
            if let Some(srv) = lookup.iter().next() {
                // Keep the trailing dot, as JNDI does.
                host = srv.target().to_string();
                port = srv.port();
                debug!(%host, port, "SRV redirect");
            }
        }
    }

    let ip = match host.parse::<IpAddr>() {
        Ok(ip) => ip,
        Err(_) => {
            let all = system_lookup(&host).await;
            // Java prefers IPv4 unless java.net.preferIPv6Addresses is set.
            all.iter()
                .find(|ip| ip.is_ipv4())
                .or(all.first())
                .copied()
                .ok_or_else(|| ResolveError::UnknownHost(host.clone()))?
        }
    };

    let handshake_host = if host.parse::<IpAddr>().is_ok() {
        reverse_confirmed(&resolver, ip).await.unwrap_or_else(|| ip.to_string())
    } else {
        host
    };

    Ok(ResolvedAddress { socket: SocketAddr::new(ip, port), handshake_host, handshake_port: port })
}

/// `InetAddress.getHostFromNameService`: PTR lookup, then accept the name
/// only if it resolves back to the same address.
async fn reverse_confirmed(resolver: &TokioResolver, ip: IpAddr) -> Option<String> {
    let lookup = resolver.reverse_lookup(ip).await.ok()?;
    let record = lookup.as_lookup().record_iter().find_map(|r| match r.data() {
        RData::PTR(ptr) => Some(ptr.0.to_string()),
        _ => None,
    })?;
    // Java returns the name without the trailing dot here.
    let name = record.trim_end_matches('.').to_owned();
    system_lookup(&name).await.contains(&ip).then_some(name)
}

/// The OS resolver (hosts file included), like `InetAddress.getByName`.
async fn system_lookup(host: &str) -> Vec<IpAddr> {
    // DNS names may end in a dot; the OS resolver accepts either form.
    match tokio::net::lookup_host((host, 0)).await {
        Ok(addrs) => addrs.map(|a| a.ip()).collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        assert_eq!(parse_address("example.net").unwrap(), ("example.net".into(), 25565));
        assert_eq!(parse_address("example.net:25566").unwrap(), ("example.net".into(), 25566));
        assert_eq!(parse_address("[::1]:1234").unwrap(), ("::1".into(), 1234));
        assert_eq!(parse_address("::1").unwrap(), ("::1".into(), 25565));
        assert!(parse_address(":25565").is_err());
        assert!(parse_address("host:notaport").is_err());
    }
}
