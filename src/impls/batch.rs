use harmony_rust_sdk::api::{
    batch::{batch_service_server::BatchService, *},
    exports::{
        hrpc::{exports::hyper, server::MakeHrpcService, HrpcService},
        prost::bytes::Bytes,
    },
};
use hyper::{header, http::HeaderValue};
use tower::Service;

use super::prelude::*;

enum Endpoint {
    Same(String),
    Different(Vec<String>),
}

fn is_valid_endpoint(endpoint: &str) -> bool {
    let endpoint = endpoint.trim_end_matches('/');
    !(endpoint.ends_with("Batch") || endpoint.ends_with("BatchSame"))
}

pub struct BatchServer<Producer: MakeHrpcService> {
    disable_ratelimits: bool,
    service: Producer,
}

impl<Producer: MakeHrpcService> BatchServer<Producer> {
    pub fn new(deps: &Dependencies, service: Producer) -> Self {
        Self {
            disable_ratelimits: deps.config.policy.disable_ratelimits,
            service,
        }
    }

    async fn make_req(
        &self,
        bodies: Vec<Bytes>,
        endpoint: Endpoint,
        auth_header: Option<HeaderValue>,
    ) -> Result<Vec<Bytes>, HrpcServerError> {
        async fn process_request(
            body: Bytes,
            endpoint: &str,
            auth_header: &Option<HeaderValue>,
            service: Option<&mut HrpcService>,
        ) -> Result<Bytes, HrpcServerError> {
            if let Some(service) = service {
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
            } else {
                Err((http::StatusCode::NOT_FOUND, "batch endpoint not found").into())
            }
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
                let req = http::Request::builder()
                    .uri(endpoint)
                    .body(hyper::Body::empty())
                    .unwrap();
                let mut service = self.service.make_hrpc_service(&req);
                for body in bodies {
                    responses.push(
                        process_request(body, endpoint, auth_header, service.as_mut()).await?,
                    );
                }
            }
            Endpoint::Different(a) => {
                for (body, endpoint) in bodies.into_iter().zip(a) {
                    tracing::info!("batching request for endpoint {}", endpoint);
                    if !is_valid_endpoint(endpoint) {
                        return Err(ServerError::InvalidBatchEndpoint.into());
                    }
                    let req = http::Request::builder()
                        .uri(endpoint)
                        .body(hyper::Body::empty())
                        .unwrap();
                    let mut service = self.service.make_hrpc_service(&req);
                    responses.push(
                        process_request(body, endpoint, auth_header, service.as_mut()).await?,
                    );
                }
            }
        }

        Ok(responses)
    }
}

#[async_trait]
impl<Producer: MakeHrpcService> BatchService for BatchServer<Producer> {
    #[rate(5, 5)]
    async fn batch(
        &self,
        mut request: Request<BatchRequest>,
    ) -> ServerResult<Response<BatchResponse>> {
        let auth_header = request.header_map_mut().remove(&header::AUTHORIZATION);
        let BatchRequest { requests } = request.into_message().await?;

        let request_len = requests.len();
        let (bodies, endpoints) = requests.into_iter().fold(
            (
                Vec::with_capacity(request_len),
                Vec::with_capacity(request_len),
            ),
            |(mut bodies, mut endpoints), request| {
                bodies.push(request.request);
                endpoints.push(request.endpoint);
                (bodies, endpoints)
            },
        );
        let responses = self
            .make_req(bodies, Endpoint::Different(endpoints), auth_header)
            .await?;

        Ok((BatchResponse { responses }).into_response())
    }

    #[rate(5, 5)]
    async fn batch_same(
        &self,
        mut request: Request<BatchSameRequest>,
    ) -> ServerResult<Response<BatchSameResponse>> {
        let auth_header = request.header_map_mut().remove(&header::AUTHORIZATION);
        let BatchSameRequest { endpoint, requests } = request.into_message().await?;

        let responses = self
            .make_req(requests, Endpoint::Same(endpoint), auth_header)
            .await?;

        Ok((BatchSameResponse { responses }).into_response())
    }
}
