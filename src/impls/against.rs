use std::convert::Infallible;

use harmony_rust_sdk::api::exports::hrpc::{
    body::box_body,
    exports::futures_util::FutureExt,
    server::{gen_prelude::BoxFuture, handler::Handler, prelude::CustomError},
};
use hyper::header::HeaderName;
use tower::Layer;

use super::*;

#[derive(Clone)]
pub struct AgainstLayer {
    http: HttpClient,
    header_name: HeaderName,
}

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
    type Service = Handler;

    fn layer(&self, mut inner: S) -> Self::Service {
        let http = self.http.clone();
        let header_name = self.header_name.clone();
        let service = service_fn(
            move |request: HttpRequest| -> BoxFuture<'static, Result<HttpResponse, Infallible>> {
                let http = http.clone();
                let maybe_host_id = request.headers().get(&header_name).and_then(|header| {
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

                    Box::pin(http.request(request).map(|res| {
                        Ok(res
                            .map(|r| r.map(box_body))
                            .unwrap_or_else(|err| ServerError::HttpError(err).as_error_response()))
                    }))
                } else {
                    tower::Service::call(&mut inner, request)
                }
            },
        );
        Handler::new(service)
    }
}

impl Default for AgainstLayer {
    fn default() -> AgainstLayer {
        AgainstLayer {
            http: http_client(),
            header_name: HeaderName::from_static("against"),
        }
    }
}
