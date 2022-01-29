use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use harmony_rust_sdk::api::exports::hrpc::{
    client::transport::http::hyper::{http_client, HttpClient},
    server::transport::http::{box_body, HttpRequest, HttpResponse},
};
use hrpc::exports::futures_util::ready;
use hyper::{client::ResponseFuture, header::HeaderName};
use pin_project::pin_project;
use tower::{Layer, Service};

use super::*;

#[derive(Clone)]
pub struct AgainstLayer;

impl<S> Layer<S> for AgainstLayer
where
    S: tower::Service<HttpRequest, Response = HttpResponse, Error = Infallible> + Send + 'static,
{
    type Service = AgainstService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AgainstService {
            http: http_client(&mut hyper::Client::builder()),
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
    S: tower::Service<HttpRequest, Response = HttpResponse, Error = Infallible> + Send + 'static,
{
    type Response = HttpResponse;

    type Error = Infallible;

    type Future = AgainstFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
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

            AgainstFuture::Remote(self.http.request(request))
        } else {
            AgainstFuture::Local(tower::Service::call(&mut self.inner, request))
        }
    }
}

#[pin_project(project = EnumProj)]
pub enum AgainstFuture<Fut> {
    Remote(#[pin] ResponseFuture),
    Local(#[pin] Fut),
}

impl<Fut> Future for AgainstFuture<Fut>
where
    Fut: Future<Output = Result<HttpResponse, Infallible>>,
{
    type Output = Result<HttpResponse, Infallible>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        match this {
            EnumProj::Local(fut) => fut.poll(cx),
            EnumProj::Remote(fut) => {
                let res = ready!(fut.poll(cx));
                Poll::Ready(Ok(res.map(|r| r.map(box_body)).unwrap_or_else(|err| {
                    ServerError::HttpError(err).into_http_response()
                })))
            }
        }
    }
}
