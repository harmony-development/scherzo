use std::{future::Future, ops::Not};

use crate::api::{
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
        fn process_request(
            body: Bytes,
            endpoint: &str,
            auth_header: &Option<HeaderValue>,
            service: &mut RoutesFinalized,
        ) -> impl Future<Output = Result<Bytes, HrpcServerError>> + Send + 'static {
            let mut req = Request::new_with_body(Body::full(body));
            *req.endpoint_mut() = endpoint.to_string().into();

            if let Some(auth) = auth_header {
                req.get_or_insert_header_map()
                    .insert(header::AUTHORIZATION, auth.clone());
            }

            let fut = service.call(req);

            async move {
                let mut reply = fut.await.unwrap();
                if let Some(err) = reply.extensions_mut().remove::<HrpcError>() {
                    bail!(err);
                }

                // This should be safe since we know we insert a message into responses
                // as whole chunks
                Ok(response::Parts::from(reply).body.next().await.unwrap()?)
            }
        }

        if self.bodies.len() > 64 {
            return Err(ServerError::TooManyBatchedRequests.into());
        }

        let mut responses = Vec::with_capacity(self.bodies.len());

        let auth_header = &self.auth_header;
        match &self.endpoint {
            Endpoint::Same(endpoint) => {
                tracing::debug!(
                    "batching {} requests for endpoint {}",
                    self.bodies.len(),
                    endpoint
                );
                if is_valid_endpoint(endpoint).not() {
                    bail!(ServerError::InvalidBatchEndpoint(endpoint.to_string()));
                }
                let mut handles = Vec::with_capacity(self.bodies.len());
                for body in self.bodies {
                    let handle =
                        tokio::spawn(process_request(body, endpoint, auth_header, service));
                    handles.push(handle);
                }
                for handle in handles {
                    responses.push(handle.await.expect("task panicked")?);
                }
            }
            Endpoint::Different(a) => {
                for endpoint in a {
                    if is_valid_endpoint(endpoint).not() {
                        bail!(ServerError::InvalidBatchEndpoint(endpoint.to_string()));
                    }
                }
                let mut handles = Vec::with_capacity(self.bodies.len());
                for (body, endpoint) in self.bodies.into_iter().zip(a) {
                    tracing::debug!("batching request for endpoint {}", endpoint);
                    let handle =
                        tokio::spawn(process_request(body, endpoint, auth_header, service));
                    handles.push(handle);
                }
                for handle in handles {
                    responses.push(handle.await.expect("task panicked")?);
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
    const ACCEPTED_ENDPOINTS: [&str; 9] = [
        "/protocol.profile.v1.ProfileService/GetProfile",
        "/protocol.chat.v1.ChatService/QueryHasPermission",
        "/protocol.chat.v1.ChatService/GetUserRoles",
        "/protocol.chat.v1.ChatService/GetGuildRoles",
        "/protocol.chat.v1.ChatService/GetGuild",
        "/protocol.chat.v1.ChatService/GetGuildChannels",
        "/protocol.mediaproxy.v1.MediaProxyService/FetchLinkMetadata",
        "/protocol.mediaproxy.v1.MediaProxyService/InstantView",
        "/protocol.mediaproxy.v1.MediaProxyService/CanInstantView",
    ];

    let endpoint = endpoint.trim_end_matches('/');
    ACCEPTED_ENDPOINTS.contains(&endpoint)
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
            disable_ratelimits: deps.config.policy.ratelimit.disable,
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
        #[rate(10, 4)]
        batch, BatchRequest, BatchResponse;
        #[rate(10, 4)]
        batch_same, BatchSameRequest, BatchSameResponse;
    }
}
