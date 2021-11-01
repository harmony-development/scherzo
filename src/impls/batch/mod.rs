use std::convert::Infallible;

use deadpool::managed::{self, Pool};
use harmony_rust_sdk::api::{
    batch::{batch_service_server::BatchService, *},
    exports::{
        hrpc::{
            exports::hyper,
            server::{gen_prelude::BoxFuture, router::RoutesFinalized, Service},
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

enum Endpoint {
    Same(String),
    Different(Vec<String>),
}

fn is_valid_endpoint(endpoint: &str) -> bool {
    let endpoint = endpoint.trim_end_matches('/');
    !(endpoint.ends_with("Batch") || endpoint.ends_with("BatchSame"))
}

#[derive(Clone)]
struct SvcManager<Svc: Service + Sync> {
    svc: Svc,
}

impl<Svc: Service + Sync> managed::Manager for SvcManager<Svc> {
    type Type = RoutesFinalized;
    type Error = Infallible;

    fn create<'life0, 'async_trait>(
        &'life0 self,
    ) -> BoxFuture<'async_trait, Result<Self::Type, Self::Error>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(std::future::ready(Ok(self.svc.make_routes().build())))
    }

    fn recycle<'life0, 'life1, 'async_trait>(
        &'life0 self,
        _obj: &'life1 mut Self::Type,
    ) -> BoxFuture<'async_trait, managed::RecycleResult<Self::Error>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(std::future::ready(Ok(())))
    }
}

pub struct BatchServer<Svc: Service + Sync> {
    disable_ratelimits: bool,
    svc_pool: Pool<SvcManager<Svc>>,
}

impl<Svc: Service + Sync + Clone> Clone for BatchServer<Svc> {
    fn clone(&self) -> Self {
        Self {
            disable_ratelimits: self.disable_ratelimits,
            svc_pool: self.svc_pool.clone(),
        }
    }
}

impl<Svc: Service + Sync> BatchServer<Svc> {
    pub fn new(deps: &Dependencies, svc: Svc) -> Self {
        Self {
            disable_ratelimits: deps.config.policy.disable_ratelimits,
            svc_pool: Pool::builder(SvcManager { svc }).build().unwrap(),
        }
    }

    async fn make_req(
        &self,
        bodies: Vec<Bytes>,
        endpoint: Endpoint,
        auth_header: Option<HeaderValue>,
    ) -> ServerResult<Vec<Bytes>> {
        let mut service = self.svc_pool.get().await.unwrap();
        (BatchReq {
            bodies,
            endpoint,
            auth_header,
        })
        .process_req(&mut service)
        .await
    }
}

impl<Svc: Service + Sync + Clone> BatchService for BatchServer<Svc> {
    impl_unary_handlers! {
        #[rate(5, 5)]
        batch, BatchRequest, BatchResponse;
        #[rate(5, 5)]
        batch_same, BatchSameRequest, BatchSameResponse;
    }
}
