use crate::ServerError;
use harmony_rust_sdk::api::{
    batch::{batch_service_server::BatchService, *},
    exports::{
        hrpc::{
            server::{prelude::BoxedFilter, ServerError as HrpcServerError},
            warp::{self, Reply},
            Request,
        },
        prost::bytes::Bytes,
    },
};
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use scherzo_derive::*;
use triomphe::Arc;

use super::Dependencies;

enum Endpoint {
    Same(String),
    Different(Vec<String>),
}

fn is_valid_endpoint(endpoint: &str) -> bool {
    let endpoint = endpoint.trim_end_matches('/');
    !(endpoint.ends_with("Batch") || endpoint.ends_with("BatchSame"))
}

pub struct BatchServer<R: Reply + 'static> {
    disable_ratelimits: bool,
    filters: Arc<BoxedFilter<(R,)>>,
}

impl<R: Reply + 'static> BatchServer<R> {
    pub fn new(deps: &Dependencies, filters: BoxedFilter<(R,)>) -> Self {
        Self {
            disable_ratelimits: deps.config.disable_ratelimits,
            filters: Arc::new(filters),
        }
    }

    async fn make_req(
        &self,
        bodies: Vec<Bytes>,
        endpoint: Endpoint,
        auth_header: Option<HeaderValue>,
    ) -> Result<Vec<Bytes>, HrpcServerError<ServerError>> {
        async fn process_request<R: Reply + 'static>(
            body: Bytes,
            endpoint: &str,
            auth_header: &Option<HeaderValue>,
            filters: &BoxedFilter<(R,)>,
        ) -> Result<Bytes, HrpcServerError<ServerError>> {
            let mut req = warp::test::request()
                .header(CONTENT_TYPE, unsafe {
                    HeaderValue::from_maybe_shared_unchecked(Bytes::from_static(
                        b"application/hrpc",
                    ))
                })
                .method("POST")
                .body(body)
                .path(endpoint);
            if let Some(auth) = auth_header {
                req = req.header(AUTHORIZATION, auth.clone());
            }

            let reply = req.filter(filters).await.map_err(HrpcServerError::Warp)?;
            let body = warp::hyper::body::to_bytes(reply.into_response().into_body())
                .await
                .unwrap();

            Ok(body)
        }

        if bodies.len() > 64 {
            return Err(ServerError::TooManyBatchedRequests.into());
        }

        let filters = self.filters.clone();
        let handle = tokio::runtime::Handle::current();

        std::thread::spawn(move || {
            handle.block_on(async move {
                let mut responses = Vec::with_capacity(bodies.len());

                let filters = filters.as_ref();
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
                            responses
                                .push(process_request(body, endpoint, auth_header, filters).await?);
                        }
                    }
                    Endpoint::Different(a) => {
                        for (body, endpoint) in bodies.into_iter().zip(a) {
                            tracing::info!("batching request for endpoint {}", endpoint);
                            if !is_valid_endpoint(endpoint) {
                                return Err(ServerError::InvalidBatchEndpoint.into());
                            }
                            responses
                                .push(process_request(body, endpoint, auth_header, filters).await?);
                        }
                    }
                }

                Ok(responses)
            })
        })
        .join()
        .unwrap()
    }
}

#[harmony_rust_sdk::api::exports::hrpc::async_trait]
impl<R: Reply + 'static> BatchService for BatchServer<R> {
    type Error = ServerError;

    #[rate(5, 5)]
    async fn batch(
        &self,
        request: Request<BatchRequest>,
    ) -> Result<BatchResponse, HrpcServerError<Self::Error>> {
        let (body, mut headers, _) = request.into_parts();
        let BatchRequest { requests } = body.into_message().await??;

        let auth_header = headers.remove(&AUTHORIZATION);
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

        Ok(BatchResponse { responses })
    }

    #[rate(5, 5)]
    async fn batch_same(
        &self,
        request: Request<BatchSameRequest>,
    ) -> Result<BatchSameResponse, HrpcServerError<Self::Error>> {
        let (body, mut headers, _) = request.into_parts();
        let BatchSameRequest { endpoint, requests } = body.into_message().await??;

        let auth_header = headers.remove(&AUTHORIZATION);
        let responses = self
            .make_req(requests, Endpoint::Same(endpoint), auth_header)
            .await?;

        Ok(BatchSameResponse { responses })
    }
}
