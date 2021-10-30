use harmony_rust_sdk::api::{
    batch::{batch_service_server::BatchService, *},
    exports::{
        hrpc::{
            exports::hyper,
            server::{router::RoutesFinalized, Service},
        },
        prost::bytes::Bytes,
    },
};
use hyper::{header, http::HeaderValue};
use tower::Service as _;

use super::prelude::*;

#[allow(clippy::module_inception)]
pub mod batch;
pub mod batch_same;

enum Endpoint {
    Same(String),
    Different(Vec<String>),
}

fn is_valid_endpoint(endpoint: &str) -> bool {
    let endpoint = endpoint.trim_end_matches('/');
    !(endpoint.ends_with("Batch") || endpoint.ends_with("BatchSame"))
}

pub struct BatchServer<MkRouter: Service + Sync> {
    disable_ratelimits: bool,
    mk_router: Arc<MkRouter>,
    router: RoutesFinalized,
}

impl<MkRouter: Service + Sync> Clone for BatchServer<MkRouter> {
    fn clone(&self) -> Self {
        Self {
            disable_ratelimits: self.disable_ratelimits,
            mk_router: self.mk_router.clone(),
            router: self.mk_router.make_routes().build(),
        }
    }
}

impl<MkRouter: Service + Sync> BatchServer<MkRouter> {
    pub fn new(deps: &Dependencies, mk_router: MkRouter) -> Self {
        Self {
            disable_ratelimits: deps.config.policy.disable_ratelimits,
            router: mk_router.make_routes().build(),
            mk_router: Arc::new(mk_router),
        }
    }

    async fn make_req(
        &mut self,
        bodies: Vec<Bytes>,
        endpoint: Endpoint,
        auth_header: Option<HeaderValue>,
    ) -> Result<Vec<Bytes>, HrpcServerError> {
        async fn process_request(
            body: Bytes,
            endpoint: &str,
            auth_header: &Option<HeaderValue>,
            service: &mut RoutesFinalized,
        ) -> Result<Bytes, HrpcServerError> {
            let mut req = http::Request::builder()
                .header(header::CONTENT_TYPE, unsafe {
                    HeaderValue::from_maybe_shared_unchecked(Bytes::from_static(
                        b"application/hrpc",
                    ))
                })
                .method(http::Method::POST)
                .uri(endpoint)
                .body(hyper::Body::from(body))
                .unwrap();
            if let Some(auth) = auth_header {
                req.headers_mut()
                    .insert(header::AUTHORIZATION, auth.clone());
            }

            let reply = service.call(req).await.unwrap();
            let body = hyper::body::to_bytes(reply.into_body()).await.unwrap();

            Ok(body)
        }

        if bodies.len() > 64 {
            return Err(ServerError::TooManyBatchedRequests.into());
        }

        let mut responses = Vec::with_capacity(bodies.len());

        let auth_header = &auth_header;
        match &endpoint {
            Endpoint::Same(endpoint) => {
                tracing::info!(
                    "batching {} requests for endpoint {}",
                    bodies.len(),
                    endpoint
                );
                if !is_valid_endpoint(endpoint) {
                    return Err(ServerError::InvalidBatchEndpoint.into());
                }
                for body in bodies {
                    responses.push(
                        process_request(body, endpoint, auth_header, &mut self.router).await?,
                    );
                }
            }
            Endpoint::Different(a) => {
                for (body, endpoint) in bodies.into_iter().zip(a) {
                    tracing::info!("batching request for endpoint {}", endpoint);
                    if !is_valid_endpoint(endpoint) {
                        return Err(ServerError::InvalidBatchEndpoint.into());
                    }
                    responses.push(
                        process_request(body, endpoint, auth_header, &mut self.router).await?,
                    );
                }
            }
        }

        Ok(responses)
    }
}

impl<MkRouter: Service + Sync> BatchService for BatchServer<MkRouter> {
    impl_unary_handlers! {
        #[rate(5, 5)]
        batch, BatchRequest, BatchResponse;
        #[rate(5, 5)]
        batch_same, BatchSameRequest, BatchSameResponse;
    }
}
