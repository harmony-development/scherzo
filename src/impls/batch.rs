use crate::ServerError;
use harmony_rust_sdk::api::{
    batch::{batch_service_server::BatchService, *},
    exports::hrpc::{
        server::{prelude::BoxedFilter, ServerError as HrpcServerError},
        warp::{self, Reply},
        Request,
    },
};
use reqwest::header::{HeaderValue, AUTHORIZATION};
use scherzo_derive::*;

use super::Dependencies;

pub struct BatchServer<R: Reply + 'static> {
    disable_ratelimits: bool,
    filters: BoxedFilter<(R,)>,
}

impl<R: Reply + 'static> BatchServer<R> {
    pub fn new(deps: &Dependencies, filters: BoxedFilter<(R,)>) -> Self {
        Self {
            disable_ratelimits: deps.config.disable_ratelimits,
            filters,
        }
    }

    async fn make_req(
        &self,
        body: Vec<u8>,
        path: &str,
        auth_header: Option<HeaderValue>,
    ) -> Result<Vec<u8>, ServerError> {
        let mut req = warp::test::request().method("POST").body(body).path(path);
        if let Some(auth) = auth_header {
            req = req.header(AUTHORIZATION, auth);
        }

        let resp = req.reply(&self.filters).await;

        // TODO: propagate errors to client
        resp.status()
            .is_success()
            .then(|| resp.into_body().to_vec())
            .ok_or(ServerError::InternalServerError)
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
                .make_req(
                    request.request,
                    request.endpoint.as_str(),
                    auth_header.clone(),
                )
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
                .make_req(request, endpoint.as_str(), auth_header.clone())
                .await?;
            responses.push(resp);
        }

        Ok(BatchSameResponse { responses })
    }
}
