use crate::ServerError;
use harmony_rust_sdk::api::{
    batch::{batch_service_server::BatchService, *},
    exports::hrpc::{server::ServerError as HrpcServerError, Request},
};
use reqwest::{
    header::{HeaderValue, AUTHORIZATION},
    Url,
};
use scherzo_derive::*;

use super::Dependencies;

pub struct BatchServer {
    http: reqwest::Client,
    local_url: Url,
    disable_ratelimits: bool,
}

impl BatchServer {
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            local_url: format!(
                "https://{}:{}",
                deps.config
                    .listen_on_localhost
                    .then(|| "127.0.0.0")
                    .unwrap_or("0.0.0.0"),
                deps.config.port
            )
            .parse()
            .unwrap(),
            disable_ratelimits: deps.config.disable_ratelimits,
            http: reqwest::Client::new(),
        }
    }

    async fn make_req(
        &self,
        body: Vec<u8>,
        endpoint: Url,
        auth_header: Option<HeaderValue>,
    ) -> Result<Vec<u8>, ServerError> {
        // TODO: propagate errors to client
        let mut req = reqwest::Request::new(reqwest::Method::POST, endpoint);
        if let Some(auth) = auth_header {
            req.headers_mut().insert(AUTHORIZATION, auth);
        }
        *req.body_mut() = Some(body.into());

        let resp = self.http.execute(req).await?.error_for_status().unwrap();
        Ok(resp.bytes().await?.to_vec())
    }
}

#[harmony_rust_sdk::api::exports::hrpc::async_trait]
impl BatchService for BatchServer {
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
                    // TODO: handle invalid url error
                    self.local_url.join(request.endpoint.as_str()).unwrap(),
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
        // TODO: handle invalid url error
        let url = self.local_url.join(endpoint.as_str()).unwrap();
        for request in requests {
            let resp = self
                .make_req(request, url.clone(), auth_header.clone())
                .await?;
            responses.push(resp);
        }

        Ok(BatchSameResponse { responses })
    }
}
