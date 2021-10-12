use crate::http;

use super::{get_content_length, prelude::*};

use std::{
    borrow::Cow,
    cmp,
    collections::HashMap,
    fs::Metadata,
    ops::Not,
    path::{Path, PathBuf},
    pin::Pin,
    str::FromStr,
    task::Poll,
};

use harmony_rust_sdk::api::{
    exports::{
        hrpc::{
            client::HttpClient,
            exports::futures_util::{
                future::{self, Either},
                ready, stream, FutureExt, Stream, StreamExt,
            },
        },
        prost::bytes::{Buf, Bytes, BytesMut},
    },
    rest::{extract_file_info_from_download_response, FileId},
};
use http::{HeaderValue, StatusCode, Uri};
use hyper::Body;
use sha3::Digest;
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader},
};
use tokio_util::io::poll_read_buf;
use tracing::info;
use warp::{filters::multipart::*, filters::BoxedFilter, Filter, Reply};

const SEPERATOR: u8 = b'\n';

pub mod download {
    use harmony_rust_sdk::api::exports::hrpc::{
        body::box_body,
        server::{prelude::CustomError, MakeHrpcService},
        HrpcService, HttpRequest,
    };
    use hyper::{header, Method};
    use tower::service_fn;

    use super::*;

    pub struct DownloadProducer {
        deps: Arc<Dependencies>,
    }

    impl MakeHrpcService for DownloadProducer {
        fn make_hrpc_service(&self, request: &HttpRequest) -> Option<HrpcService> {
            let maybe_file_id = request
                .uri()
                .path()
                .strip_prefix("/_harmony/media/download/")
                .map(|id| urlencoding::decode(id).unwrap_or(Cow::Borrowed(id)))
                .and_then(|id| FileId::from_str(&id).ok());

            let deps = self.deps.clone();

            if let Some(file_id) = maybe_file_id {
                let service = service_fn(move |request: HttpRequest| {
                    async fn make_request(
                        http_client: &HttpClient,
                        url: Uri,
                    ) -> Result<http::Response<Body>, ServerError> {
                        let resp = http_client.get(url).await.map_err(ServerError::from)?;

                        if resp.status().is_success().not() {
                            let err = if resp.status() == StatusCode::NOT_FOUND {
                                ServerError::MediaNotFound
                            } else {
                                // TODO: proper error
                                ServerError::InternalServerError
                            };
                            Err(err)
                        } else {
                            Ok(resp)
                        }
                    }

                    let deps = deps.clone();
                    let file_id = file_id.clone();

                    async move {
                        if request.method() != Method::GET {
                            return Ok((StatusCode::METHOD_NOT_ALLOWED, "method must be get")
                                .as_error_response());
                        }

                        let media_root = deps.config.media.media_root.as_path();
                        let http_client = &deps.http;
                        let host = &deps.config.host;

                        let (content_disposition, content_type, content_body, content_length) =
                            match file_id {
                                FileId::External(url) => {
                                    info!("Serving external image from {}", url);
                                    let filename =
                                        url.path().split('/').last().unwrap_or("unknown");
                                    let disposition = unsafe { disposition_header(filename) };
                                    let resp = match make_request(http_client, url).await {
                                        Ok(resp) => resp,
                                        Err(err) => return Ok(err.as_error_response()),
                                    };
                                    let content_type = resp
                                        .headers()
                                        .get(&http::header::CONTENT_TYPE)
                                        .and_then(|v| {
                                            const ALLOWED_TYPES: [&[u8]; 3] =
                                                [b"image", b"audio", b"video"];
                                            const LEN: usize = 5;
                                            let compare = |t: &[u8]| {
                                                t.iter()
                                                    .zip(v.as_bytes().iter().take(LEN))
                                                    .all(|(a, b)| a == b)
                                            };
                                            (v.len() > LEN
                                                && std::array::IntoIter::new(ALLOWED_TYPES)
                                                    .any(compare))
                                            .then(|| v.clone())
                                        });

                                    if let Some(content_type) = content_type {
                                        let len = get_content_length(&resp);
                                        (disposition, content_type, resp.into_body(), len)
                                    } else {
                                        return Ok(ServerError::NotMedia.as_error_response());
                                    }
                                }
                                FileId::Hmc(hmc) => {
                                    info!("Serving HMC from {}", hmc);
                                    if format!("{}:{}", hmc.server(), hmc.port()) == host.as_str() {
                                        info!("Serving local media with id {}", hmc.id());
                                        match get_file(media_root, hmc.id()).await {
                                            Ok(data) => data,
                                            Err(err) => return Ok(err.as_error_response()),
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
                                            Err(err) => return Ok(err.as_error_response()),
                                        };

                                        let extract_result =
                                            extract_file_info_from_download_response(
                                                resp.headers(),
                                            )
                                            .map(|(name, mimetype, _)| {
                                                (
                                                    unsafe { disposition_header(name) },
                                                    mimetype.clone(),
                                                )
                                            })
                                            .map_err(
                                                |e| ServerError::FileExtractUnexpected(e.into()),
                                            );
                                        let (disposition, mimetype) = match extract_result {
                                            Ok(data) => data,
                                            Err(err) => return Ok(err.as_error_response()),
                                        };
                                        let len = get_content_length(&resp);
                                        (disposition, mimetype, resp.into_body(), len)
                                    }
                                }
                                FileId::Id(id) => {
                                    info!("Serving local media with id {}", id);
                                    match get_file(media_root, &id).await {
                                        Ok(data) => data,
                                        Err(err) => return Ok(err.as_error_response()),
                                    }
                                }
                            };

                        Ok(http::Response::builder()
                            .header(header::CONTENT_TYPE, content_type)
                            .header(header::CONTENT_DISPOSITION, content_disposition)
                            .header(header::CONTENT_LENGTH, content_length)
                            .body(box_body(content_body))
                            .unwrap())
                    }
                });
                return Some(HrpcService::new(service));
            }

            None
        }
    }

    pub fn producer(deps: Arc<Dependencies>) -> DownloadProducer {
        DownloadProducer { deps }
    }
}

pub fn upload(
    sessions: SessionMap,
    media_root: Arc<PathBuf>,
    max_length: u64,
) -> BoxedFilter<(impl Reply,)> {
    warp::post()
        .and(warp::filters::header::value("authorization").and_then(
            move |maybe_token: HeaderValue| {
                let res = maybe_token
                    .to_str()
                    .map(|token| {
                        sessions
                            .contains_key(token)
                            .then(|| ())
                            .ok_or(ServerError::Unauthenticated)
                    })
                    .map_err(|_| ServerError::InvalidAuthId)
                    .and_then(std::convert::identity)
                    .map_err(reject);
                future::ready(res)
            },
        ))
        .untuple_one()
        .and(warp::query::<HashMap<SmolStr, SmolStr>>())
        .and(form().max_length(max_length))
        .and_then(
            move |param: HashMap<SmolStr, SmolStr>, mut form: FormData| {
                let media_root = media_root.clone();
                async move {
                    if let Some(res) = form.next().await {
                        let mut part = res.map_err(reject)?;
                        let data = part
                            .data()
                            .await
                            .ok_or_else(|| reject(ServerError::MissingFiles))?
                            .map_err(reject)?;
                        let data = data.chunk();
                        let name = param.get("filename").map(SmolStr::as_str);
                        let content_type = param.get("contentType").map(SmolStr::as_str);

                        let id = write_file(media_root.as_path(), data, name, content_type, None)
                            .await
                            .map_err(reject)?;

                        Ok(format!(r#"{{ "id": "{}" }}"#, id))
                    } else {
                        Err(reject(ServerError::MissingFiles))
                    }
                }
            },
        )
        .boxed()
}

#[inline(always)]
pub fn reject(err: impl Into<ServerError>) -> warp::Rejection {
    warp::reject::custom(err.into())
}

// Safety: the `name` argument MUST ONLY contain ASCII characters.
unsafe fn disposition_header(name: &str) -> HeaderValue {
    HeaderValue::from_maybe_shared_unchecked(Bytes::from(
        format!("attachment; filename={}", name).into_bytes(),
    ))
}

pub async fn write_file(
    media_root: &Path,
    data: &[u8],
    name: Option<&str>,
    content_type: Option<&str>,
    write_to: Option<PathBuf>,
) -> Result<String, ServerError> {
    let id = format!("{:x}", sha3::Sha3_256::digest(data));
    let path = write_to.unwrap_or_else(|| media_root.join(&id));
    if path.exists() {
        return Ok(id);
    }
    // [tag:ascii_filename_upload]
    let name = name.unwrap_or("unknown");
    // [tag:ascii_mimetype_upload]
    let content_type = content_type
        .or_else(|| infer::get(data).map(|t| t.mime_type()))
        .unwrap_or("application/octet-stream");
    let data = [
        name.as_bytes(),
        &[SEPERATOR],
        content_type.as_bytes(),
        &[SEPERATOR],
        data,
    ]
    .concat();

    tokio::fs::write(path, data).await?;

    Ok(id)
}

pub async fn get_file_handle(media_root: &Path, id: &str) -> Result<(File, Metadata), ServerError> {
    let file_path = media_root.join(id);
    let file = tokio::fs::File::open(file_path).await.map_err(|err| {
        if let std::io::ErrorKind::NotFound = err.kind() {
            ServerError::MediaNotFound
        } else {
            err.into()
        }
    })?;
    let metadata = file.metadata().await?;
    Ok((file, metadata))
}

pub async fn read_bufs(
    file: &mut File,
    is_jpeg: bool,
) -> Result<(Vec<u8>, Vec<u8>, BufReader<&mut File>), ServerError> {
    let mut buf_reader = BufReader::new(file);

    if is_jpeg {
        Ok((b"unknown.jpg".to_vec(), b"image/jpeg".to_vec(), buf_reader))
    } else {
        let mut filename_raw = Vec::with_capacity(20);
        buf_reader.read_until(SEPERATOR, &mut filename_raw).await?;
        filename_raw.pop();

        let mut mimetype_raw = Vec::with_capacity(20);
        buf_reader.read_until(SEPERATOR, &mut mimetype_raw).await?;
        mimetype_raw.pop();

        Ok((filename_raw, mimetype_raw, buf_reader))
    }
}

pub async fn get_file_full(
    media_root: &Path,
    id: &str,
) -> Result<(String, String, Vec<u8>, u64), ServerError> {
    let is_jpeg = is_id_jpeg(id);
    let (mut file, metadata) = get_file_handle(media_root, id).await?;
    let (filename_raw, mimetype_raw, mut buf_reader) = read_bufs(&mut file, is_jpeg).await?;

    let (start, end) = calculate_range(&filename_raw, &mimetype_raw, &metadata, is_jpeg);
    let size = end - start;
    let mut file_raw = Vec::with_capacity(size as usize);
    buf_reader.read_to_end(&mut file_raw).await?;

    unsafe {
        Ok((
            String::from_utf8_unchecked(filename_raw),
            String::from_utf8_unchecked(mimetype_raw),
            file_raw,
            size,
        ))
    }
}

pub fn calculate_range(
    filename_raw: &[u8],
    mimetype_raw: &[u8],
    metadata: &Metadata,
    is_jpeg: bool,
) -> (u64, u64) {
    // + 2 is because we need to factor in the 2 b'\n' seperators
    let start = is_jpeg
        .then(|| 0)
        .unwrap_or_else(|| (filename_raw.len() + mimetype_raw.len()) as u64 + 2);
    let end = metadata.len();

    (start, end)
}

pub fn is_id_jpeg(id: &str) -> bool {
    id.ends_with("_jpeg")
}

pub async fn get_file(
    media_root: &Path,
    id: &str,
) -> Result<(HeaderValue, HeaderValue, warp::hyper::Body, HeaderValue), ServerError> {
    let is_jpeg = is_id_jpeg(id);
    let (mut file, metadata) = get_file_handle(media_root, id).await?;
    let (filename_raw, mimetype_raw, _) = read_bufs(&mut file, is_jpeg).await?;

    let (start, end) = calculate_range(&filename_raw, &mimetype_raw, &metadata, is_jpeg);
    let mimetype: Bytes = mimetype_raw.into();

    let disposition = unsafe {
        let filename = std::str::from_utf8_unchecked(&filename_raw);
        // Safety: filenames must be valid ASCII characters since we get them through only ASCII allowed structures [ref:ascii_filename_upload]
        disposition_header(filename)
    };
    // Safety: mimetypes must be valid ASCII chars since we get them through only ASCII allowed structures [ref:ascii_mimetype_upload]
    let mimetype = unsafe { HeaderValue::from_maybe_shared_unchecked(mimetype) };

    let buf_size = optimal_buf_size(&metadata);
    Ok((
        disposition,
        mimetype,
        warp::hyper::Body::wrap_stream(file_stream(file, buf_size, (start, end))),
        unsafe {
            HeaderValue::from_maybe_shared_unchecked(Bytes::from(
                (end - start).to_string().into_bytes(),
            ))
        },
    ))
}

fn file_stream(
    mut file: tokio::fs::File,
    buf_size: usize,
    (start, end): (u64, u64),
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    use std::io::SeekFrom;

    let seek = async move {
        // We will always seek from a point that is non-zero
        file.seek(SeekFrom::Start(start)).await?;
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
