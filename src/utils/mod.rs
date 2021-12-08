use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    str::FromStr,
    time::Duration,
};

use hrpc::{request::BoxRequest, server::layer::ratelimit::RateLimitLayer};

pub mod either;
pub mod evec;

pub fn rate_limit(
    num: u64,
    per: Duration,
    check_header_for_ip: Option<String>,
    allowed_ips: Option<Vec<String>>,
) -> RateLimitLayer<
    impl Fn(&mut BoxRequest) -> Option<IpAddr> + Clone,
    impl Fn(&IpAddr) -> bool + Clone,
> {
    let allowed_ips = allowed_ips.map(|ips| {
        ips.into_iter()
            .map(|s| IpAddr::from_str(&s))
            .flatten()
            .collect::<HashSet<_, ahash::RandomState>>()
    });

    RateLimitLayer::new(num, per).set_key_fns(
        move |req| {
            check_header_for_ip
                .as_deref()
                .and_then(|header_name| get_forwarded_for_ip_addr(req, header_name))
                .or_else(|| get_ip_addr(req))
        },
        move |ip| allowed_ips.as_ref().map_or(false, |ips| ips.contains(ip)),
    )
}

fn get_ip_addr(req: &BoxRequest) -> Option<IpAddr> {
    req.extensions().get::<SocketAddr>().map(|addr| addr.ip())
}

fn get_forwarded_for_ip_addr(req: &BoxRequest, check_header_for_ip: &str) -> Option<IpAddr> {
    req.header_map()
        .and_then(|headers| headers.get(check_header_for_ip))
        .and_then(|val| val.to_str().ok())
        .and_then(|ips| ips.split(',').map(str::trim).next())
        .and_then(|ip_raw| IpAddr::from_str(ip_raw).ok())
}
