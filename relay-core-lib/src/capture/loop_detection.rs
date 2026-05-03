use std::net::{SocketAddr, IpAddr};
use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, RwLock};
use if_addrs::get_if_addrs;

pub struct LoopDetector {
    /// All addresses the proxy is listening on
    listen_addrs: BTreeSet<SocketAddr>,
    
    /// Local interface addresses (updated periodically)
    local_addrs: Arc<RwLock<BTreeSet<IpAddr>>>,
}

impl LoopDetector {
    pub fn new(listen_addrs: BTreeSet<SocketAddr>) -> Self {
        Self {
            listen_addrs,
            local_addrs: Arc::new(RwLock::new(BTreeSet::new())),
        }
    }

    /// Check if connecting to target would create a loop
    pub fn would_loop(&self, target: SocketAddr) -> bool {
        // Direct match with listening addresses
        if self.listen_addrs.contains(&target) {
            return true;
        }
        
        // Check if target IP is local/loopback and port matches any listen port.
        if self.is_local_ip(target.ip()) {
             let listen_ports: HashSet<u16> = self.listen_addrs.iter()
                .map(|a| a.port())
                .collect();
            if listen_ports.contains(&target.port()) {
                return true;
            }
        }
        
        false
    }
    
    fn is_local_ip(&self, ip: IpAddr) -> bool {
        if ip.is_loopback() {
            return true;
        }
        if ip.is_unspecified() {
            return true;
        }
        
        // Check cached local addrs
        if let Ok(guard) = self.local_addrs.read() {
            if guard.contains(&ip) {
                return true;
            }
        }
        
        false
    }
    
    /// Refresh local interface addresses from system interfaces.
    pub async fn refresh_local_addrs(&self) {
        let mut ips = BTreeSet::new();

        if let Ok(ifaces) = get_if_addrs() {
            for iface in ifaces {
                ips.insert(iface.ip());
            }
        }

        // Keep loopback and unspecified in cache as a defensive fallback.
        ips.insert(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        ips.insert(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST));
        ips.insert(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
        ips.insert(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED));

        if let Ok(mut guard) = self.local_addrs.write() {
            *guard = ips;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_would_loop_on_direct_listen_match() {
        let listen = BTreeSet::from([SocketAddr::from_str("127.0.0.1:8080").expect("addr")]);
        let detector = LoopDetector::new(listen);
        let target = SocketAddr::from_str("127.0.0.1:8080").expect("addr");
        assert!(detector.would_loop(target));
    }

    #[test]
    fn test_would_loop_on_cached_local_ip_and_listen_port() {
        let listen = BTreeSet::from([SocketAddr::from_str("0.0.0.0:9090").expect("addr")]);
        let detector = LoopDetector::new(listen);

        if let Ok(mut guard) = detector.local_addrs.write() {
            guard.insert(IpAddr::from_str("10.10.10.1").expect("ip"));
        }

        let target = SocketAddr::from_str("10.10.10.1:9090").expect("addr");
        assert!(detector.would_loop(target));
    }

    #[tokio::test]
    async fn test_refresh_local_addrs_populates_cache() {
        let detector = LoopDetector::new(BTreeSet::new());
        detector.refresh_local_addrs().await;

        let guard = detector.local_addrs.read().expect("lock");
        assert!(
            !guard.is_empty(),
            "refresh should populate at least fallback local addresses"
        );
    }
}
