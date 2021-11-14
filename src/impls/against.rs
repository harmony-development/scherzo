use std::convert::Infallible;

use harmony_rust_sdk::api::exports::hrpc::{
    common::transport::http::box_body,
    exports::futures_util::{future::BoxFuture, FutureExt},
};
use hrpc::common::transport::http::{HttpRequest, HttpResponse};
use hyper::header::HeaderName;
use tower::{Layer, Service};

use super::*;

#[derive(Clone)]
pub struct AgainstLayer;

impl<S> Layer<S> for AgainstLayer
where
    S: tower::Service<
            HttpRequest,
            Response = HttpResponse,
            Error = Infallible,
            Future = BoxFuture<'static, Result<HttpResponse, Infallible>>,
        > + Send
        + 'static,
{
    type Service = AgainstService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AgainstService {
            http: http_client(),
            header_name: HeaderName::from_static("against"),
            inner,
        }
    }
}

pub struct AgainstService<S> {
    http: HttpClient,
    header_name: HeaderName,
    inner: S,
}

impl<S> Service<HttpRequest> for AgainstService<S>
where
    S: tower::Service<
            HttpRequest,
            Response = HttpResponse,
            Error = Infallible,
            Future = BoxFuture<'static, Result<HttpResponse, Infallible>>,
        > + Send
        + 'static,
{
    type Response = HttpResponse;

    type Error = Infallible;

    type Future = BoxFuture<'static, Result<HttpResponse, Infallible>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Service::poll_ready(&mut self.inner, cx)
    }

    fn call(&mut self, request: HttpRequest) -> Self::Future {
        let maybe_host_id = request.headers().get(&self.header_name).and_then(|header| {
            header
                .to_str()
                .ok()
                .and_then(|v| HomeserverIdentifier::from_str(v).ok())
        });
        if let Some(host_id) = maybe_host_id {
            let url = {
                let mut url_parts = host_id.to_url().into_parts();
                url_parts.path_and_query = Some(request.uri().path().parse().unwrap());
                Uri::from_parts(url_parts).unwrap()
            };
            let (parts, body) = request.into_parts();

            let mut request = http::Request::builder()
                .uri(url)
                .method(parts.method)
                .body(body)
                .unwrap();
            *request.headers_mut() = parts.headers;
            *request.extensions_mut() = parts.extensions;

            Box::pin(self.http.request(request).map(|res| {
                Ok(res
                    .map(|r| r.map(box_body))
                    .unwrap_or_else(|err| ServerError::HttpError(err).into_http_response()))
            }))
        } else {
            tower::Service::call(&mut self.inner, request)
        }
    }
}
