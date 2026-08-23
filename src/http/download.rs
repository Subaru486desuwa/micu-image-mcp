use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use async_trait::async_trait;
use url::{Host, Url};

use crate::config::Config;

#[derive(Clone, Debug)]
pub struct ResolvedDownload {
    pub url: Url,
    pub host: String,
    pub port: u16,
    pub addresses: Vec<IpAddr>,
}

#[async_trait]
pub trait Resolver: Send + Sync {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, String>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemResolver;

#[async_trait]
impl Resolver for SystemResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, String> {
        let resolved = tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| format!("下载 URL host 解析失败 {host:?}: {error}"))?;
        let mut addresses = resolved.map(|socket| socket.ip()).collect::<Vec<_>>();
        addresses.sort();
        addresses.dedup();
        Ok(addresses)
    }
}

pub async fn validate_download_url<R: Resolver + ?Sized>(
    config: &Config,
    raw_url: &str,
    resolver: &R,
) -> Result<ResolvedDownload, String> {
    let url = Url::parse(raw_url).map_err(|error| format!("下载 URL 解析失败: {error:?}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "下载 URL scheme 非法（仅允许 http/https）: {}",
            python_string_repr(url.scheme())
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("下载 URL 不允许包含 username/password".into());
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "下载 URL 缺少有效 port".to_owned())?;
    let (host, mut addresses) = match url.host() {
        Some(Host::Ipv4(ip)) => (ip.to_string(), vec![IpAddr::V4(ip)]),
        Some(Host::Ipv6(ip)) => (ip.to_string(), vec![IpAddr::V6(ip)]),
        Some(Host::Domain(domain)) => {
            let normalized = domain.trim_end_matches('.').to_ascii_lowercase();
            let answers = resolver.resolve(&normalized, port).await?;
            (normalized, answers)
        }
        None => return Err("下载 URL 缺少 host".into()),
    };
    addresses.sort();
    addresses.dedup();
    if addresses.is_empty() {
        return Err(format!(
            "下载 URL host 无法解析: {}",
            python_string_repr(&host)
        ));
    }
    for address in &addresses {
        if ip_is_blocked(*address, Some(&host), config) {
            return Err(format!(
                "下载 URL host {} 指向受限地址 {address}（私网/环回/链路本地/保留），已拒绝（SSRF 防护）",
                python_string_repr(&host)
            ));
        }
    }
    Ok(ResolvedDownload {
        url,
        host,
        port,
        addresses,
    })
}

pub fn host_is_trusted(host: &str, trusted_hosts: &[String]) -> bool {
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    trusted_hosts.iter().any(|trusted| {
        let trusted = trusted.trim_end_matches('.').to_ascii_lowercase();
        normalized == trusted || normalized.ends_with(&format!(".{trusted}"))
    })
}

pub fn ip_is_blocked(ip: IpAddr, host: Option<&str>, config: &Config) -> bool {
    match ip {
        IpAddr::V4(ipv4) => ipv4_is_blocked(ipv4, host, config),
        IpAddr::V6(ipv6) => {
            if let Some(mapped) = ipv6.to_ipv4_mapped() {
                return ipv4_is_blocked(mapped, host, config);
            }
            ipv6_is_blocked(ipv6)
        }
    }
}

fn ipv4_is_blocked(ip: Ipv4Addr, host: Option<&str>, config: &Config) -> bool {
    if ipv4_in_network(ip, Ipv4Addr::new(198, 18, 0, 0), 15) {
        return !(config.allow_fake_ip_download
            && host.is_some_and(|name| host_is_trusted(name, &config.trusted_download_hosts)));
    }
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ipv4_in_network(ip, Ipv4Addr::new(100, 64, 0, 0), 10)
        || ipv4_in_network(ip, Ipv4Addr::new(192, 0, 0, 0), 24)
        || ipv4_in_network(ip, Ipv4Addr::new(192, 0, 2, 0), 24)
        || ipv4_in_network(ip, Ipv4Addr::new(192, 88, 99, 0), 24)
        || ipv4_in_network(ip, Ipv4Addr::new(198, 51, 100, 0), 24)
        || ipv4_in_network(ip, Ipv4Addr::new(203, 0, 113, 0), 24)
        || ipv4_in_network(ip, Ipv4Addr::new(240, 0, 0, 0), 4)
}

fn ipv6_is_blocked(ip: Ipv6Addr) -> bool {
    if ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
    {
        return true;
    }
    // Public IPv6 unicast is currently allocated from 2000::/3.  Reject other special-use
    // prefixes and selected documentation/transition ranges inside it.
    !ipv6_in_network(ip, Ipv6Addr::new(0x2000, 0, 0, 0, 0, 0, 0, 0), 3)
        || ipv6_in_network(ip, Ipv6Addr::new(0x2001, 0x0002, 0, 0, 0, 0, 0, 0), 48)
        || ipv6_in_network(ip, Ipv6Addr::new(0x2001, 0x0010, 0, 0, 0, 0, 0, 0), 28)
        || ipv6_in_network(ip, Ipv6Addr::new(0x2001, 0x0020, 0, 0, 0, 0, 0, 0), 28)
        || ipv6_in_network(ip, Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 0), 32)
        || ipv6_in_network(ip, Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16)
        || ipv6_in_network(ip, Ipv6Addr::new(0x3fff, 0, 0, 0, 0, 0, 0, 0), 20)
}

fn ipv4_in_network(ip: Ipv4Addr, network: Ipv4Addr, prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    (u32::from(ip) & mask) == (u32::from(network) & mask)
}

fn ipv6_in_network(ip: Ipv6Addr, network: Ipv6Addr, prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    };
    (u128::from(ip) & mask) == (u128::from(network) & mask)
}

fn python_string_repr(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        net::{IpAddr, Ipv4Addr},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::*;

    #[derive(Clone)]
    struct FakeResolver {
        answers: Vec<IpAddr>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Resolver for FakeResolver {
        async fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<IpAddr>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.answers.clone())
        }
    }

    fn config() -> Config {
        Config::from_map(&BTreeMap::from([
            ("HOME".into(), "/tmp/micu-download-home".into()),
            ("MICU_SAVE_DIR_ROOT".into(), "/tmp/micu-download-out".into()),
            (
                "MICU_TRUSTED_DOWNLOAD_HOSTS".into(),
                "oss.filenest.top".into(),
            ),
        ]))
        .unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn ip_policy_blocks_private_reserved_and_mapped_addresses() {
        let cfg = config();
        for raw in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "192.168.1.1",
            "0.0.0.0",
            "224.0.0.1",
            "192.0.2.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "2001:db8::1",
        ] {
            let ip = raw.parse().unwrap_or_else(|error| panic!("{error}"));
            assert!(ip_is_blocked(ip, Some("example.test"), &cfg), "{raw}");
        }
        for raw in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            let ip = raw.parse().unwrap_or_else(|error| panic!("{error}"));
            assert!(!ip_is_blocked(ip, Some("example.test"), &cfg), "{raw}");
        }
    }

    #[test]
    fn fake_ip_requires_both_trusted_host_and_enabled_flag() {
        let cfg = config();
        let fake = IpAddr::V4(Ipv4Addr::new(198, 18, 1, 23));
        assert!(!ip_is_blocked(fake, Some("oss.filenest.top"), &cfg));
        assert!(!ip_is_blocked(fake, Some("cdn.oss.filenest.top"), &cfg));
        assert!(ip_is_blocked(fake, Some("evil.example"), &cfg));
        assert!(ip_is_blocked(fake, None, &cfg));
    }

    #[tokio::test]
    async fn url_validation_resolves_once_and_returns_addresses_for_pinning() {
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = FakeResolver {
            answers: vec![
                "93.184.216.34"
                    .parse()
                    .unwrap_or_else(|error| panic!("{error}")),
            ],
            calls: calls.clone(),
        };
        let resolved =
            validate_download_url(&config(), "https://images.example.test/x.png", &resolver)
                .await
                .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(resolved.host, "images.example.test");
        assert_eq!(resolved.port, 443);
        assert_eq!(resolved.addresses, resolver.answers);
    }

    #[tokio::test]
    async fn url_validation_rejects_scheme_credentials_and_any_private_dns_answer() {
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = FakeResolver {
            answers: vec![
                "93.184.216.34"
                    .parse()
                    .unwrap_or_else(|error| panic!("{error}")),
                "10.0.0.1".parse().unwrap_or_else(|error| panic!("{error}")),
            ],
            calls,
        };
        assert!(
            validate_download_url(&config(), "file:///etc/passwd", &resolver)
                .await
                .is_err()
        );
        assert!(
            validate_download_url(&config(), "https://user:pass@example.test/x", &resolver)
                .await
                .is_err()
        );
        assert!(
            validate_download_url(&config(), "https://example.test/x", &resolver)
                .await
                .is_err_and(|error| error.contains("受限地址"))
        );
    }
}
