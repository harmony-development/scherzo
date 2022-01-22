use crate::{http, utils::http_ratelimit::RateLimit};

use self::{about::AboutService, download::DownloadService, upload::UploadService};

use super::{gen_rand_inline_str, get_content_length, prelude::*};

use std::{
    borrow::Cow,
    cmp,
    convert::Infallible,
    fs::Metadata,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    str::FromStr,
    task::{Context, Poll},
    time::Duration,
};

use harmony_rust_sdk::api::{
    exports::{
        hrpc::{
            exports::futures_util::{
                future::{self, BoxFuture, Either},
                ready, stream, FutureExt, Stream, StreamExt,
            },
            server::transport::http::{box_body, HttpRequest, HttpResponse},
        },
        prost::bytes::{Bytes, BytesMut},
    },
    rest::{extract_file_info_from_download_response, FileId},
};
use hrpc::common::future::Ready;
use http::{header, HeaderValue, Method, StatusCode, Uri};
use hyper::Body;
use pin_project::pin_project;
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, BufWriter},
};
use tokio_util::io::poll_read_buf;
use tower::{Layer, Service};
use tracing::info;

pub mod about;
pub mod download;
pub mod upload;

const SEPERATOR: u8 = b'\n';

type Out = Result<HttpResponse, Infallible>;

#[derive(Clone)]
pub struct RestServiceLayer {
    deps: Arc<Dependencies>,
}

impl RestServiceLayer {
    pub fn new(deps: Arc<Dependencies>) -> Self {
        Self { deps }
    }
}

impl<S> Layer<S> for RestServiceLayer
where
    S: tower::Service<HttpRequest, Response = HttpResponse, Error = Infallible> + Send + 'static,
{
    type Service = RestService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RestService {
            download: download::handler(self.deps.clone()),
            upload: upload::handler(self.deps.clone()),
            about: about::handler(self.deps.clone()),
            inner,
        }
    }
}

pub struct RestService<S> {
    download: RateLimit<DownloadService>,
    upload: RateLimit<UploadService>,
    about: RateLimit<AboutService>,
    inner: S,
}

impl<S> Service<HttpRequest> for RestService<S>
where
    S: tower::Service<HttpRequest, Response = HttpResponse, Error = Infallible> + Send + 'static,
{
    type Response = HttpResponse;

    type Error = Infallible;

    type Future = RestFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let pending = Service::poll_ready(&mut self.inner, cx).is_pending()
            | Service::poll_ready(&mut self.about, cx).is_pending()
            | Service::poll_ready(&mut self.download, cx).is_pending()
            | Service::poll_ready(&mut self.upload, cx).is_pending();

        pending
            .then(|| Poll::Pending)
            .unwrap_or(Poll::Ready(Ok(())))
    }

    fn call(&mut self, req: HttpRequest) -> Self::Future {
        let path = req.uri().path();

        if path.starts_with("/_harmony/media/download/") {
            RestFuture::Other(Service::call(&mut self.download, req))
        } else {
            match path {
                "/_harmony/media/upload" => RestFuture::Other(Service::call(&mut self.upload, req)),
                "/_harmony/about" => RestFuture::About(Service::call(&mut self.about, req)),
                _ => RestFuture::Inner(Service::call(&mut self.inner, req)),
            }
        }
    }
}

#[pin_project(project = EnumProj)]
pub enum RestFuture<Fut> {
    Inner(#[pin] Fut),
    About(crate::utils::http_ratelimit::RateLimitFuture<Ready<Out>>),
    Other(crate::utils::http_ratelimit::RateLimitFuture<BoxFuture<'static, Out>>),
}

impl<Fut> Future for RestFuture<Fut>
where
    Fut: Future<Output = Out>,
{
    type Output = Out;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        match this {
            EnumProj::About(fut) => fut.poll_unpin(cx),
            EnumProj::Other(fut) => fut.poll_unpin(cx),
            EnumProj::Inner(fut) => Future::poll(fut, cx),
        }
    }
}
