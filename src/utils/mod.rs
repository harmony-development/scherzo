use std::{
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
) -> RateLimitLayer<impl Fn(&mut BoxRequest) -> Option<IpAddr> + Clone> {
    RateLimitLayer::new(num, per).extract_key_with(move |req| {
        check_header_for_ip
            .as_deref()
            .and_then(|header_name| get_forwarded_for_ip_addr(req, header_name))
            .or_else(|| get_ip_addr(req))
    })
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
