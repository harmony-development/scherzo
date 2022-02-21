use std::{cmp, convert::Infallible, fs::Metadata, ops::Not};

use hrpc::{
    exports::futures_util::{
        future::{self, BoxFuture, Either},
        ready, stream, FutureExt, Stream, StreamExt,
    },
    server::transport::http::HttpResponse,
};
use prost::bytes::BytesMut;
use reqwest::Client as HttpClient;
use tokio::io::AsyncSeekExt;
use tokio_util::io::poll_read_buf;
use tower::Service;

use crate::{impls::media::FileHandle, rest_error_response};

use super::*;

pub struct DownloadService {
    deps: Arc<Dependencies>,
}

impl Service<HttpRequest> for DownloadService {
    type Response = HttpResponse;

    type Error = Infallible;

    type Future = BoxFuture<'static, Result<HttpResponse, Infallible>>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, request: HttpRequest) -> Self::Future {
        async fn make_request(
            http_client: &HttpClient,
            url: String,
        ) -> Result<reqwest::Response, ServerError> {
            let resp = http_client
                .get(url)
                .send()
                .await
                .map_err(ServerError::FailedToDownload)?;

            if resp.status().is_success().not() && resp.status() == StatusCode::NOT_FOUND {
                return Err(ServerError::MediaNotFound);
            }

            resp.error_for_status()
                .map_err(ServerError::FailedToDownload)
        }

        let deps = self.deps.clone();
        let maybe_file_id = request
            .uri()
            .path()
            .strip_prefix("/_harmony/media/download/")
            .map(|id| urlencoding::decode(id).unwrap_or(Cow::Borrowed(id)))
            .and_then(|id| FileId::from_str(&id).ok());

        let fut = async move {
            if request.method() != Method::GET {
                return Ok(rest_error_response(
                    "method must be GET".to_string(),
                    StatusCode::METHOD_NOT_ALLOWED,
                ));
            }

            let file_id = match maybe_file_id {
                Some(file_id) => file_id,
                _ => return Ok(ServerError::InvalidFileId.into_rest_http_response()),
            };

            let http_client = &deps.http;
            let host = &deps.config.host;

            let (content_disposition, content_type, content_body, content_length) = match file_id {
                FileId::External(url) => {
                    info!("Serving external image from {}", url);
                    let filename = url.path().split('/').last().unwrap_or("unknown");
                    let disposition = unsafe { disposition_header(filename) };
                    let resp = match make_request(http_client, url.to_string()).await {
                        Ok(resp) => resp,
                        Err(err) => return Ok(err.into_rest_http_response()),
                    };
                    let content_type =
                        resp.headers()
                            .get(&http::header::CONTENT_TYPE)
                            .and_then(|v| {
                                const ALLOWED_TYPES: [&[u8]; 3] = [b"image", b"audio", b"video"];
                                const LEN: usize = 5;
                                let compare = |t: &[u8]| {
                                    t.iter()
                                        .zip(v.as_bytes().iter().take(LEN))
                                        .all(|(a, b)| a == b)
                                };
                                (v.len() > LEN && ALLOWED_TYPES.into_iter().any(compare))
                                    .then(|| v.clone())
                            });

                    if let Some(content_type) = content_type {
                        let len = get_content_length(resp.headers());
                        (
                            disposition,
                            content_type,
                            Body::wrap_stream(resp.bytes_stream()),
                            len,
                        )
                    } else {
                        return Ok(ServerError::NotMedia.into_rest_http_response());
                    }
                }
                FileId::Hmc(hmc) => {
                    info!("Serving HMC from {}", hmc);
                    if format!("{}:{}", hmc.server(), hmc.port()) == host.as_str() {
                        info!("Serving local media with id {}", hmc.id());
                        match deps.media.get_file(hmc.id()).await {
                            Ok(handle) => file_handle_to_data(handle),
                            Err(err) => return Ok(err.into_rest_http_response()),
                        }
                    } else {
                        // Safety: this is always valid, since HMC is a valid URL
                        let url = unsafe {
                            format!(
                                "https://{}:{}/_harmony/media/download/{}",
                                hmc.server(),
                                hmc.port(),
                                hmc.id()
                            )
                            .parse()
                            .unwrap_unchecked()
                        };
                        let resp = match make_request(http_client, url).await {
                            Ok(resp) => resp,
                            Err(err) => return Ok(err.into_rest_http_response()),
                        };

                        let extract_result =
                            extract_file_info_from_download_response(resp.headers())
                                .map(|(name, mimetype, _)| {
                                    (unsafe { disposition_header(name) }, mimetype.clone())
                                })
                                .map_err(|e| ServerError::FileExtractUnexpected(e.into()));
                        let (disposition, mimetype) = match extract_result {
                            Ok(data) => data,
                            Err(err) => return Ok(err.into_rest_http_response()),
                        };
                        let len = get_content_length(resp.headers());
                        (
                            disposition,
                            mimetype,
                            Body::wrap_stream(resp.bytes_stream()),
                            len,
                        )
                    }
                }
                FileId::Id(id) => {
                    info!("Serving local media with id {}", id);
                    match deps.media.get_file(&id).await {
                        Ok(handle) => file_handle_to_data(handle),
                        Err(err) => return Ok(err.into_rest_http_response()),
                    }
                }
            };

            Ok(http::Response::builder()
                .header(header::CONTENT_TYPE, content_type)
                .header(header::CONTENT_DISPOSITION, content_disposition)
                .header(header::CONTENT_LENGTH, content_length)
                .body(box_body(content_body))
                .unwrap())
        };

        Box::pin(fut)
    }
}

pub fn handler(deps: Arc<Dependencies>) -> RateLimit<DownloadService> {
    let client_ip_header_name = deps.config.policy.ratelimit.client_ip_header_name.clone();
    let allowed_ips = deps.config.policy.ratelimit.allowed_ips.clone();
    RateLimit::new(
        DownloadService { deps },
        30,
        Duration::from_secs(5),
        client_ip_header_name,
        allowed_ips,
    )
}

fn file_handle_to_data(handle: FileHandle) -> (HeaderValue, HeaderValue, Body, HeaderValue) {
    let stream = Body::wrap_stream(file_stream(
        handle.file,
        optimal_buf_size(&handle.metadata),
        handle.range,
    ));
    let disposition = unsafe { disposition_header(&handle.name) };
    let mime = unsafe { string_header(handle.mime) };
    let size = unsafe { string_header(handle.size.to_string()) };
    (disposition, mime, stream, size)
}

// SAFETY: the `name` argument MUST ONLY contain ASCII characters.
unsafe fn disposition_header(name: &str) -> HeaderValue {
    string_header(format!("inline; filename={}", name))
}

// SAFETY: the `string` argument MUST ONLY contain ASCII characters.
unsafe fn string_header(string: String) -> HeaderValue {
    HeaderValue::from_maybe_shared_unchecked(Bytes::from(string.into_bytes()))
}

fn file_stream(
    mut file: tokio::fs::File,
    buf_size: usize,
    (start, end): (u64, u64),
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    use std::io::SeekFrom;

    let seek = async move {
        if start != 0 {
            file.seek(SeekFrom::Start(start)).await?;
        }
        Ok(file)
    };

    seek.into_stream()
        .map(move |result| {
            let mut buf = BytesMut::new();
            let mut len = end - start;
            let mut f = match result {
                Ok(f) => f,
                Err(f) => return Either::Left(stream::once(future::err(f))),
            };

            Either::Right(stream::poll_fn(move |cx| {
                if len == 0 {
                    return Poll::Ready(None);
                }
                reserve_at_least(&mut buf, buf_size);

                let n = match ready!(poll_read_buf(Pin::new(&mut f), cx, &mut buf)) {
                    Ok(n) => n as u64,
                    Err(err) => {
                        tracing::debug!("file read error: {}", err);
                        return Poll::Ready(Some(Err(err)));
                    }
                };

                if n == 0 {
                    tracing::debug!("file read found EOF before expected length");
                    return Poll::Ready(None);
                }

                let mut chunk = buf.split().freeze();
                if n > len {
                    chunk = chunk.split_to(len as usize);
                    len = 0;
                } else {
                    len -= n;
                }

                Poll::Ready(Some(Ok(chunk)))
            }))
        })
        .flatten()
}

fn reserve_at_least(buf: &mut BytesMut, cap: usize) {
    if buf.capacity() - buf.len() < cap {
        buf.reserve(cap);
    }
}

fn optimal_buf_size(metadata: &Metadata) -> usize {
    let block_size = get_block_size(metadata);

    // If file length is smaller than block size, don't waste space
    // reserving a bigger-than-needed buffer.
    cmp::min(block_size as u64, metadata.len()) as usize
}

#[cfg(unix)]
fn get_block_size(metadata: &Metadata) -> usize {
    use std::os::unix::fs::MetadataExt;
    //TODO: blksize() returns u64, should handle bad cast...
    //(really, a block size bigger than 4gb?)

    // Use device blocksize unless it's really small.
    cmp::max(metadata.blksize() as usize, DEFAULT_READ_BUF_SIZE)
}

#[cfg(not(unix))]
#[inline(always)]
const fn get_block_size(_metadata: &Metadata) -> usize {
    DEFAULT_READ_BUF_SIZE
}

const DEFAULT_READ_BUF_SIZE: usize = 8_192;
