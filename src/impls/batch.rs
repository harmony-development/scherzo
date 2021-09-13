use crate::ServerError;
use harmony_rust_sdk::api::{
    batch::{batch_service_server::BatchService, *},
    exports::{
        hrpc::{
            server::{prelude::BoxedFilter, ServerError as HrpcServerError},
            warp::{self, Reply},
            Request,
        },
        prost::bytes::Buf,
    },
};
use reqwest::header::{HeaderValue, AUTHORIZATION};
use scherzo_derive::*;
use triomphe::Arc;

use super::Dependencies;

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
        requests: Vec<AnyRequest>,
        auth_header: Option<HeaderValue>,
    ) -> Result<Vec<Vec<u8>>, HrpcServerError<ServerError>> {
        let filters = self.filters.clone();
        let handle = tokio::runtime::Handle::current();

        std::thread::spawn(move || {
            handle.block_on(async move {
                let mut responses = Vec::with_capacity(requests.len());

                for request in requests {
                    let mut req = warp::test::request()
                        .method("POST")
                        .body(request.request)
                        .path(request.endpoint.as_str());
                    if let Some(auth) = auth_header.clone() {
                        req = req.header(AUTHORIZATION, auth);
                    }

                    let reply = req
                        .filter(filters.as_ref())
                        .await
                        .map_err(HrpcServerError::Warp)?;
                    let body = warp::hyper::body::aggregate(reply.into_response().into_body())
                        .await
                        .unwrap()
                        .chunk()
                        .to_vec();

                    responses.push(body);
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
        let responses = self.make_req(requests, auth_header).await?;

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
            .make_req(
                requests
                    .into_iter()
                    .map(|a| AnyRequest {
                        request: a,
                        endpoint: endpoint.clone(),
                    })
                    .collect(),
                auth_header,
            )
            .await?;

        Ok(BatchSameResponse { responses })
    }
}
