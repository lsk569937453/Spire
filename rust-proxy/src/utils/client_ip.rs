use http::header::HeaderMap;
use ipnet::IpNet;
use std::net::{IpAddr, SocketAddr};

/// Returns the address the gateway should treat as the client.
///
/// `X-Forwarded-For` is only honoured when the direct peer is a *trusted* proxy
/// (a configured CIDR, normally the load balancer in front of Spire). Without
/// that check any client could set the header itself and bypass IP based rate
/// limiting, IP banning and allow/deny lists — or, worse, get somebody else
/// banned.
pub fn resolve_client_ip(peer: SocketAddr, headers: &HeaderMap, trusted: &[IpNet]) -> IpAddr {
    if trusted.is_empty() || !trusted.iter().any(|net| net.contains(&peer.ip())) {
        return peer.ip();
    }
    forwarded_client_ip(headers, trusted).unwrap_or_else(|| peer.ip())
}

/// Reads the client address from `X-Forwarded-For`, walking the chain from the
/// right (closest hop) to the left and skipping addresses that are themselves
/// trusted proxies.
fn forwarded_client_ip(headers: &HeaderMap, trusted: &[IpNet]) -> Option<IpAddr> {
    let raw = headers.get("x-forwarded-for")?.to_str().ok()?;
    let chain: Vec<IpAddr> = raw
        .split(',')
        .filter_map(|entry| entry.trim().parse::<IpAddr>().ok())
        .collect();

    chain
        .iter()
        .rev()
        .find(|addr| !trusted.iter().any(|net| net.contains(*addr)))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn nets(entries: &[&str]) -> Vec<IpNet> {
        entries.iter().map(|e| e.parse().unwrap()).collect()
    }

    fn peer(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 443)
    }

    #[test]
    fn returns_peer_when_no_trusted_proxies_are_configured() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
        assert_eq!(
            resolve_client_ip(peer("10.0.0.1"), &headers, &[]),
            "10.0.0.1".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn ignores_spoofed_header_from_untrusted_peer() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
        let trusted = nets(&["10.0.0.0/8"]);
        assert_eq!(
            resolve_client_ip(peer("198.51.100.7"), &headers, &trusted),
            "198.51.100.7".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn uses_forwarded_for_from_trusted_proxy() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.9, 10.0.0.5"),
        );
        let trusted = nets(&["10.0.0.0/8"]);
        assert_eq!(
            resolve_client_ip(peer("10.0.0.5"), &headers, &trusted),
            "203.0.113.9".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn falls_back_to_peer_when_chain_is_all_trusted() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("10.0.0.9, 10.0.0.5"),
        );
        let trusted = nets(&["10.0.0.0/8"]);
        assert_eq!(
            resolve_client_ip(peer("10.0.0.5"), &headers, &trusted),
            "10.0.0.5".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn ignores_garbage_entries() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("not-an-ip, 203.0.113.9"),
        );
        let trusted = nets(&["10.0.0.0/8"]);
        assert_eq!(
            resolve_client_ip(peer("10.0.0.5"), &headers, &trusted),
            "203.0.113.9".parse::<IpAddr>().unwrap()
        );
    }
}
