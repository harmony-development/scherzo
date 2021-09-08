use crate::{
    http,
    impls::{auth::SessionMap, get_content_length, rate},
    ServerError,
};

use std::{
    borrow::Cow,
    cmp,
    collections::HashMap,
    fs::Metadata,
    path::{Path, PathBuf},
    pin::Pin,
    str::FromStr,
    task::Poll,
};

use futures_util::StreamExt;
use harmony_rust_sdk::api::{
    exports::{
        hrpc::{
            futures_util::{
                self,
                future::{self, Either},
                ready, stream, FutureExt, Stream,
            },
            server::ServerError as HrpcError,
            warp,
        },
        prost::bytes::{Buf, Bytes, BytesMut},
    },
    rest::{extract_file_info_from_download_response, FileId},
};
use reqwest::{header::HeaderValue, StatusCode, Url};
use sha3::Digest;
use smol_str::SmolStr;
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader},
};
use tokio_util::io::poll_read_buf;
use tracing::info;
use triomphe::Arc;
use warp::{filters::multipart::*, filters::BoxedFilter, reply::Response, Filter, Reply};

use super::Dependencies;

const SEPERATOR: u8 = b'\n';

pub fn rest(deps: &Dependencies) -> BoxedFilter<(impl Reply,)> {
    let media_conf = &deps.config.media;
    download(
        Arc::new(media_conf.media_root.clone()),
        deps.config.host.clone(),
        deps.config.disable_ratelimits,
    )
    .or(upload(
        deps.valid_sessions.clone(),
        Arc::new(media_conf.media_root.clone()),
        media_conf.max_upload_length,
        deps.config.disable_ratelimits,
    ))
    .boxed()
}

pub fn download(
    media_root: Arc<PathBuf>,
    host: String,
    disable_ratelimits: bool,
) -> BoxedFilter<(impl Reply,)> {
    let http_client = reqwest::Client::new();
    let host: SmolStr = host.into();
    warp::get()
        .and(warp::path("_harmony"))
        .and(warp::path("media"))
        .and(warp::path("download"))
        .and(warp::path::param::<String>())
        .and(
            disable_ratelimits
                .then(|| warp::any().boxed())
                .unwrap_or_else(|| rate(20, 5)),
        )
        .and_then(move |id: String| {
            async fn make_request(
                http_client: &reqwest::Client,
                url: Url,
            ) -> Result<reqwest::Response, warp::Rejection> {
                http_client
                    .get(url)
                    .send()
                    .await
                    .map_err(reject)?
                    .error_for_status()
                    .map_err(|err| {
                        if err.status().unwrap() == StatusCode::NOT_FOUND {
                            reject(ServerError::MediaNotFound)
                        } else {
                            reject(err)
                        }
                    })
            }
            let host = host.clone();
            let id = urlencoding::decode(&id).unwrap_or_else(|_| Cow::Borrowed(id.as_str()));
            let media_root = media_root.clone();
            let http_client = http_client.clone();
            let file_id = FileId::from_str(&id).map_err(|_| reject(ServerError::InvalidFileId));
            async move {
                match file_id? {
                    FileId::External(url) => {
                        info!("Serving external image from {}", url);
                        let resp = make_request(&http_client, url).await?;
                        let content_type = resp
                            .headers()
                            .get(&http::header::CONTENT_TYPE)
                            .and_then(|v| {
                                const ALLOWED_TYPES: [&[u8]; 3] = [b"image", b"audio", b"video"];
                                const LEN: usize = 5;
                                let compare = |t: &[u8]| {
                                    t.iter()
                                        .zip(v.as_bytes().iter().take(LEN))
                                        .all(|(a, b)| a == b)
                                };
                                (v.len() > LEN
                                    && std::array::IntoIter::new(ALLOWED_TYPES).any(compare))
                                .then(|| v.clone())
                            });

                        if let Some(content_type) = content_type {
                            let filename = resp
                                .url()
                                .path_segments()
                                .expect("cannot be a cannot-be-a-base url")
                                .last()
                                .unwrap_or("unknown");
                            let disposition = unsafe { disposition_header(filename) };
                            let len = get_content_length(&resp);
                            let data_stream = resp.bytes_stream();
                            Ok((
                                disposition,
                                content_type,
                                warp::hyper::Body::wrap_stream(data_stream),
                                len,
                            ))
                        } else {
                            Err(reject(ServerError::NotMedia))
                        }
                    }
                    FileId::Hmc(hmc) => {
                        info!("Serving HMC from {}", hmc);
                        if format!("{}:{}", hmc.server(), hmc.port()) == host.as_str() {
                            info!("Serving local media with id {}", hmc.id());
                            Ok(get_file(media_root.as_ref(), hmc.id())
                                .await
                                .map_err(reject)?)
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
                            let resp = make_request(&http_client, url).await?;
                            let (disposition, mimetype) =
                                extract_file_info_from_download_response(resp.headers())
                                    .map(|(name, mimetype, _)| {
                                        (unsafe { disposition_header(name) }, mimetype.clone())
                                    })
                                    .map_err(|e| reject(ServerError::Unexpected(e.into())))?;
                            let len = get_content_length(&resp);
                            let data_stream = resp.bytes_stream();
                            Ok((
                                disposition,
                                mimetype,
                                warp::hyper::Body::wrap_stream(data_stream),
                                len,
                            ))
                        }
                    }
                    FileId::Id(id) => {
                        info!("Serving local media with id {}", id);
                        get_file(media_root.as_ref(), &id).await.map_err(reject)
                    }
                }
            }
        })
        .map(
            |(disposition, content_type, data, len): (
                HeaderValue,
                HeaderValue,
                warp::hyper::Body,
                HeaderValue,
            )| {
                let mut resp = Response::new(data);
                resp.headers_mut()
                    .insert(http::header::CONTENT_TYPE, content_type);
                // TODO: content disposition attachment thingy?
                resp.headers_mut()
                    .insert(http::header::CONTENT_DISPOSITION, disposition);
                resp.headers_mut().insert(http::header::CONTENT_LENGTH, len);
                resp
            },
        )
        .boxed()
}

pub fn upload(
    sessions: SessionMap,
    media_root: Arc<PathBuf>,
    max_length: u64,
    disable_ratelimits: bool,
) -> BoxedFilter<(impl Reply,)> {
    warp::post()
        .and(warp::path!("_harmony" / "media" / "upload"))
        .and(
            disable_ratelimits
                .then(|| warp::any().boxed())
                .unwrap_or_else(|| rate(5, 5)),
        )
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
    warp::reject::custom(HrpcError::Custom(err.into()))
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
