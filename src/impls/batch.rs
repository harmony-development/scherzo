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

    async fn make_req<'a>(
        &self,
        body: Vec<u8>,
        path: String,
        auth_header: Option<HeaderValue>,
    ) -> Result<Vec<u8>, HrpcServerError<ServerError>> {
        let filters = self.filters.clone();
        std::thread::spawn(move || {
            tokio::task::block_in_place(move || {
                tokio::runtime::Handle::current().block_on(async move {
                    let mut req = warp::test::request()
                        .method("POST")
                        .body(body)
                        .path(path.as_str());
                    if let Some(auth) = auth_header {
                        req = req.header(AUTHORIZATION, auth);
                    }

                    let resp = req.filter(filters.as_ref()).await;

                    match resp {
                        Ok(r) => Ok(warp::hyper::body::aggregate(r.into_response().into_body())
                            .await
                            .unwrap()
                            .chunk()
                            .to_vec()),
                        Err(e) => Err(HrpcServerError::Warp(e)),
                    }
                })
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

        let mut responses = Vec::with_capacity(requests.len());

        let auth_header = headers.remove(&AUTHORIZATION);
        for request in requests {
            let resp = self
                .make_req(request.request, request.endpoint, auth_header.clone())
                .await?;
            responses.push(resp);
        }

        Ok(BatchResponse { responses })
    }

    #[rate(5, 5)]
    async fn batch_same(
        &self,
        request: Request<BatchSameRequest>,
    ) -> Result<BatchSameResponse, HrpcServerError<Self::Error>> {
        let (body, mut headers, _) = request.into_parts();
        let BatchSameRequest { endpoint, requests } = body.into_message().await??;

        let mut responses = Vec::with_capacity(requests.len());

        let auth_header = headers.remove(&AUTHORIZATION);

        for request in requests {
            let resp = self
                .make_req(request, endpoint.clone(), auth_header.clone())
                .await?;
            responses.push(resp);
        }

        Ok(BatchSameResponse { responses })
    }
}
