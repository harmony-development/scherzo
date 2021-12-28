use std::convert::Infallible;

use crate::{rest_error_response, SCHERZO_VERSION};

use super::*;
use harmony_rust_sdk::api::rest::About;
use hrpc::{
    common::future::{ready, Ready},
    server::transport::http::HttpResponse,
};
use tower::Service;

pub fn handler(deps: Arc<Dependencies>) -> RateLimit<AboutService> {
    let client_ip_header_name = deps.config.policy.ratelimit.client_ip_header_name.clone();
    let allowed_ips = deps.config.policy.ratelimit.allowed_ips.clone();
    RateLimit::new(
        AboutService { deps },
        3,
        Duration::from_secs(5),
        client_ip_header_name,
        allowed_ips,
    )
}

pub struct AboutService {
    deps: Arc<Dependencies>,
}

impl Service<HttpRequest> for AboutService {
    type Response = HttpResponse;

    type Error = Infallible;

    type Future = Ready<Result<HttpResponse, Infallible>>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: HttpRequest) -> Self::Future {
        if req.method() != Method::GET {
            return ready(Ok(rest_error_response(
                "method must be GET".to_string(),
                StatusCode::METHOD_NOT_ALLOWED,
            )));
        }

        let json = serde_json::to_vec(&About {
            server_name: "Scherzo".to_string(),
            version: SCHERZO_VERSION.to_string(),
            about_server: self.deps.config.server_description.clone(),
            message_of_the_day: self.deps.runtime_config.lock().motd.clone(),
        })
        .unwrap();

        ready(Ok(http::Response::builder()
            .status(StatusCode::OK)
            .header(
                http::header::CONTENT_TYPE,
                HeaderValue::from_static("text/json"),
            )
            .body(box_body(Body::from(json)))
            .unwrap()))
    }
}
