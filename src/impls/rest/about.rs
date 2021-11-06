use crate::SCHERZO_VERSION;

use super::*;
use harmony_rust_sdk::api::{exports::hrpc::server::service::HrpcService, rest::About};
use tower::limit::RateLimitLayer;

pub fn handler(deps: Arc<Dependencies>) -> HrpcService {
    let service = service_fn(move |_: HttpRequest| {
        let deps = deps.clone();
        async move {
            let json = serde_json::to_string(&About {
                server_name: "Scherzo".to_string(),
                version: SCHERZO_VERSION.to_string(),
                about_server: deps.config.server_description.clone(),
                message_of_the_day: deps.runtime_config.lock().motd.clone(),
            })
            .unwrap();

            Ok(http::Response::builder()
                .status(StatusCode::OK)
                .body(full_box_body(json.into_bytes().into()))
                .unwrap())
        }
    });
    HrpcService::new(
        ServiceBuilder::new()
            .layer(RateLimitLayer::new(3, Duration::from_secs(5)))
            .service(service),
    )
}
