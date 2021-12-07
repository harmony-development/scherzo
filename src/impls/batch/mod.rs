use harmony_rust_sdk::api::{
    batch::{batch_service_server::BatchService, *},
    exports::{
        hrpc::{
            response,
            server::{router::RoutesFinalized, MakeRoutes},
        },
        prost::bytes::Bytes,
    },
};
use hrpc::{body::Body, exports::futures_util::StreamExt};
use hyper::{header, http::HeaderValue};
use swimmer::{Pool, PoolBuilder, Recyclable};
use tower::Service as _;

use super::prelude::*;

#[allow(clippy::module_inception)]
pub mod batch;
pub mod batch_same;

struct BatchReq {
    bodies: Vec<Bytes>,
    endpoint: Endpoint,
    auth_header: Option<HeaderValue>,
}

impl BatchReq {
    async fn process_req(
        self,
        service: &mut RoutesFinalized,
    ) -> Result<Vec<Bytes>, HrpcServerError> {
        async fn process_request(
            body: Bytes,
            endpoint: &str,
            auth_header: &Option<HeaderValue>,
            service: &mut RoutesFinalized,
        ) -> Result<Bytes, HrpcServerError> {
            let mut req = Request::new_with_body(Body::full(body));
            *req.endpoint_mut() = endpoint.to_string().into();

            if let Some(auth) = auth_header {
                req.get_or_insert_header_map()
                    .insert(header::AUTHORIZATION, auth.clone());
            }

            let reply = service.call(req).await.unwrap();

            // This should be safe since we know we insert a message into responses
            // as whole chunks
            Ok(response::Parts::from(reply).body.next().await.unwrap()?)
        }

        if self.bodies.len() > 64 {
            return Err(ServerError::TooManyBatchedRequests.into());
        }

        let mut responses = Vec::with_capacity(self.bodies.len());

        let auth_header = &self.auth_header;
        match &self.endpoint {
            Endpoint::Same(endpoint) => {
                tracing::info!(
                    "batching {} requests for endpoint {}",
                    self.bodies.len(),
                    endpoint
                );
                if !is_valid_endpoint(endpoint) {
                    return Err(ServerError::InvalidBatchEndpoint.into());
                }
                for body in self.bodies {
                    responses.push(process_request(body, endpoint, auth_header, service).await?);
                }
            }
            Endpoint::Different(a) => {
                for (body, endpoint) in self.bodies.into_iter().zip(a) {
                    tracing::info!("batching request for endpoint {}", endpoint);
                    if !is_valid_endpoint(endpoint) {
                        return Err(ServerError::InvalidBatchEndpoint.into());
                    }
                    responses.push(process_request(body, endpoint, auth_header, service).await?);
                }
            }
        }

        Ok(responses)
    }
}

struct RecyclableService(RoutesFinalized);

impl Recyclable for RecyclableService {
    fn new() -> Self
    where
        Self: Sized,
    {
        unreachable!("won't panic because we use the supplier function")
    }

    fn recycle(&mut self) {}
}

enum Endpoint {
    Same(String),
    Different(Vec<String>),
}

fn is_valid_endpoint(endpoint: &str) -> bool {
    let endpoint = endpoint.trim_end_matches('/');
    !(endpoint.ends_with("Batch") || endpoint.ends_with("BatchSame"))
}

#[derive(Clone)]
pub struct BatchServer {
    deps: Arc<Dependencies>,
    disable_ratelimits: bool,
    svc_pool: Arc<Pool<RecyclableService>>,
}

impl BatchServer {
    pub fn new<Svc: MakeRoutes + Sync>(deps: Arc<Dependencies>, svc: Svc) -> Self {
        Self {
            disable_ratelimits: deps.config.policy.disable_ratelimits,
            svc_pool: Arc::new(
                PoolBuilder::default()
                    .with_supplier(move || RecyclableService(svc.make_routes().build()))
                    .build(),
            ),
            deps,
        }
    }

    async fn make_req(
        &self,
        bodies: Vec<Bytes>,
        endpoint: Endpoint,
        auth_header: Option<HeaderValue>,
    ) -> ServerResult<Vec<Bytes>> {
        let mut service = self.svc_pool.get();
        (BatchReq {
            bodies,
            endpoint,
            auth_header,
        })
        .process_req(&mut service.0)
        .await
    }
}

impl BatchService for BatchServer {
    impl_unary_handlers! {
        #[rate(5, 5)]
        batch, BatchRequest, BatchResponse;
        #[rate(5, 5)]
        batch_same, BatchSameRequest, BatchSameResponse;
    }
}
