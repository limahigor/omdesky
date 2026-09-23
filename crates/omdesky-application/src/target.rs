use omdesky_core::MeshPeer;
use std::net::IpAddr;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PeerLookupError {
    NotFound,
    Ambiguous(Vec<String>),
}

pub fn find_peer<'a>(peers: &'a [MeshPeer], target: &str) -> Result<&'a MeshPeer, PeerLookupError> {
    let target = target.trim();

    let matches = match target.parse::<IpAddr>() {
        Ok(address) => peers
            .iter()
            .filter(|peer| peer.ips.contains(&address))
            .collect::<Vec<_>>(),
        Err(_) => peers
            .iter()
            .filter(|peer| names_peer(peer, target))
            .collect::<Vec<_>>(),
    };

    match matches.as_slice() {
        [] => Err(PeerLookupError::NotFound),
        [peer] => Ok(peer),
        _ => Err(PeerLookupError::Ambiguous(
            matches.iter().map(|peer| unique_name(peer)).collect(),
        )),
    }
}

pub fn display_name(peer: &MeshPeer) -> String {
    peer.hostname
        .clone()
        .or_else(|| {
            peer.dns_name
                .as_deref()
                .map(full_dns_name)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| peer.tailnet_node_id.clone())
}

pub fn unique_name(peer: &MeshPeer) -> String {
    peer.dns_name
        .as_deref()
        .map(full_dns_name)
        .map(str::to_owned)
        .unwrap_or_else(|| peer.tailnet_node_id.clone())
}

fn names_peer(peer: &MeshPeer, target: &str) -> bool {
    if target.is_empty() {
        return false;
    }

    if peer.tailnet_node_id == target {
        return true;
    }

    let hostname_matches = peer
        .hostname
        .as_deref()
        .is_some_and(|hostname| hostname.eq_ignore_ascii_case(target));

    let dns_matches = peer.dns_name.as_deref().is_some_and(|dns_name| {
        let full = full_dns_name(dns_name);
        let short = full.split('.').next().unwrap_or(full);

        full.eq_ignore_ascii_case(target.trim_end_matches('.'))
            || short.eq_ignore_ascii_case(target)
    });

    hostname_matches || dns_matches
}

fn full_dns_name(dns_name: &str) -> &str {
    dns_name.trim_end_matches('.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use omdesky_core::ConnectionKind;
    use std::net::Ipv4Addr;

    fn peer(id: &str, hostname: &str, dns_name: &str, last_octet: u8) -> MeshPeer {
        MeshPeer {
            tailnet_node_id: id.to_owned(),
            dns_name: Some(dns_name.to_owned()),
            hostname: Some(hostname.to_owned()),
            ips: vec![IpAddr::V4(Ipv4Addr::new(100, 64, 0, last_octet))],
            online: true,
            connection: ConnectionKind::Direct,
            latency_ms: None,
        }
    }

    fn tailnet() -> Vec<MeshPeer> {
        vec![
            peer("nHOPPE", "hoppe", "hoppe.tail1234.ts.net.", 2),
            peer("nAVELL", "avell", "avell.tail1234.ts.net.", 3),
            peer("nPOCO", "POCO X3 Pro", "poco-x3-pro.tail1234.ts.net.", 4),
        ]
    }

    fn found(target: &str) -> Result<String, PeerLookupError> {
        let peers = tailnet();

        find_peer(&peers, target).map(|peer| peer.tailnet_node_id.clone())
    }

    #[test]
    fn test_the_name_tailscale_shows_selects_the_device() {
        assert_eq!(found("hoppe"), Ok("nHOPPE".to_owned()));
        assert_eq!(found("avell"), Ok("nAVELL".to_owned()));
        assert_eq!(found("POCO X3 Pro"), Ok("nPOCO".to_owned()));
    }

    #[test]
    fn test_names_ignore_letter_case() {
        assert_eq!(found("Hoppe"), Ok("nHOPPE".to_owned()));
        assert_eq!(found("poco x3 pro"), Ok("nPOCO".to_owned()));
    }

    #[test]
    fn test_a_prefix_never_selects_a_device() {
        for target in ["hop", "hopp", "av", "hoppe.tail", "POCO", ""] {
            assert_eq!(found(target), Err(PeerLookupError::NotFound), "{target}");
        }
    }

    #[test]
    fn test_the_magic_dns_name_selects_the_device() {
        assert_eq!(found("poco-x3-pro"), Ok("nPOCO".to_owned()));
        assert_eq!(found("hoppe.tail1234.ts.net"), Ok("nHOPPE".to_owned()));
        assert_eq!(found("hoppe.tail1234.ts.net."), Ok("nHOPPE".to_owned()));
    }

    #[test]
    fn test_the_stable_node_id_and_address_select_the_device() {
        assert_eq!(found("nAVELL"), Ok("nAVELL".to_owned()));
        assert_eq!(found("100.64.0.2"), Ok("nHOPPE".to_owned()));
        assert_eq!(found("100.64.0.99"), Err(PeerLookupError::NotFound));
    }

    #[test]
    fn test_two_devices_with_the_same_name_are_ambiguous() {
        let mut peers = tailnet();
        peers.push(peer("nHOPPE2", "hoppe", "hoppe-1.tail1234.ts.net.", 9));

        assert_eq!(
            find_peer(&peers, "hoppe").map(|peer| peer.tailnet_node_id.clone()),
            Err(PeerLookupError::Ambiguous(vec![
                "hoppe.tail1234.ts.net".to_owned(),
                "hoppe-1.tail1234.ts.net".to_owned()
            ]))
        );
        assert_eq!(
            find_peer(&peers, "hoppe-1").map(|peer| peer.tailnet_node_id.clone()),
            Ok("nHOPPE2".to_owned())
        );
    }

    #[test]
    fn test_offline_devices_are_still_found() {
        let mut peers = tailnet();
        peers[0].online = false;

        assert!(!find_peer(&peers, "hoppe").expect("found").online);
    }
}
