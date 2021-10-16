use crate::http;

use super::{gen_rand_inline_str, get_content_length, prelude::*};

use std::{
    borrow::Cow,
    cmp,
    fs::Metadata,
    ops::Not,
    path::{Path, PathBuf},
    pin::Pin,
    str::FromStr,
    task::Poll,
    time::Duration,
};

use harmony_rust_sdk::api::{
    exports::{
        hrpc::{
            body::{box_body, full_box_body},
            client::HttpClient,
            exports::futures_util::{
                future::{self, Either},
                ready, stream, FutureExt, Stream, StreamExt,
            },
            return_err_as_resp,
            server::{prelude::CustomError, RouterBuilder, Server},
            HttpRequest,
        },
        prost::bytes::{Bytes, BytesMut},
    },
    rest::{extract_file_info_from_download_response, FileId},
};
use http::{header, HeaderValue, Method, StatusCode, Uri};
use hyper::Body;
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, BufWriter},
};
use tokio_util::io::poll_read_buf;
use tower::{service_fn, ServiceBuilder};
use tracing::info;

const SEPERATOR: u8 = b'\n';

pub struct MediaProducer {
    deps: Arc<Dependencies>,
}

impl MediaProducer {
    pub fn new(deps: Arc<Dependencies>) -> Self {
        Self { deps }
    }
}

impl Server for MediaProducer {
    fn make_router(&self) -> RouterBuilder {
        let deps = self.deps.clone();

        let download = service_fn(move |request: HttpRequest| {
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
            let maybe_file_id = request
                .uri()
                .path()
                .strip_prefix("/_harmony/media/download/")
                .map(|id| urlencoding::decode(id).unwrap_or(Cow::Borrowed(id)))
                .and_then(|id| FileId::from_str(&id).ok());

            async move {
                if request.method() != Method::GET {
                    return Ok(
                        (StatusCode::METHOD_NOT_ALLOWED, "method must be get").as_error_response()
                    );
                }

                let file_id = match maybe_file_id {
                    Some(file_id) => file_id,
                    _ => return Ok(ServerError::InvalidFileId.as_error_response()),
                };

                let media_root = deps.config.media.media_root.as_path();
                let http_client = &deps.http;
                let host = &deps.config.host;

                let (content_disposition, content_type, content_body, content_length) =
                    match file_id {
                        FileId::External(url) => {
                            info!("Serving external image from {}", url);
                            let filename = url.path().split('/').last().unwrap_or("unknown");
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
                                        && std::array::IntoIter::new(ALLOWED_TYPES).any(compare))
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
                                    extract_file_info_from_download_response(resp.headers())
                                        .map(|(name, mimetype, _)| {
                                            (unsafe { disposition_header(name) }, mimetype.clone())
                                        })
                                        .map_err(|e| ServerError::FileExtractUnexpected(e.into()));
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
        let download = ServiceBuilder::new()
            .rate_limit(10, Duration::from_secs(5))
            .service(download);

        let deps = self.deps.clone();
        let upload = service_fn(move |request: HttpRequest| {
            let deps = deps.clone();

            let auth_res = deps.valid_sessions.auth_header_map(request.headers());

            async move {
                if let Err(err) = auth_res {
                    return Ok(err.into_response());
                }
                let boundary_res = request
                    .headers()
                    .get(&header::CONTENT_TYPE)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|v| multer::parse_boundary(v).ok())
                    .ok_or((
                        StatusCode::BAD_REQUEST,
                        "content_type header not found or was invalid",
                    ));
                let boundary = return_err_as_resp!(boundary_res);
                let mut multipart = multer::Multipart::with_constraints(
                    request.into_body(),
                    boundary,
                    multer::Constraints::new()
                        .allowed_fields(vec!["file"])
                        .size_limit(
                            multer::SizeLimit::new()
                                .whole_stream(deps.config.media.max_upload_length),
                        ),
                );

                match multipart.next_field().await {
                    Ok(maybe_field) => match maybe_field {
                        Some(field) => {
                            let id = return_err_as_resp!(
                                write_file(deps.config.media.media_root.as_path(), field, None)
                                    .await
                            );

                            Ok(http::Response::builder()
                                .status(StatusCode::OK)
                                .body(full_box_body(
                                    format!(r#"{{ "id": "{}" }}"#, id).into_bytes().into(),
                                ))
                                .unwrap())
                        }
                        None => Ok(ServerError::MissingFiles.as_error_response()),
                    },
                    Err(err) => Ok(ServerError::from(err).as_error_response()),
                }
            }
        });
        let upload = ServiceBuilder::new()
            .rate_limit(3, Duration::from_secs(5))
            .service(upload);

        RouterBuilder::new()
            .route("/_harmony/media/download/:id", download)
            .route("/_harmony/media/upload", upload)
    }
}

// Safety: the `name` argument MUST ONLY contain ASCII characters.
unsafe fn disposition_header(name: &str) -> HeaderValue {
    HeaderValue::from_maybe_shared_unchecked(Bytes::from(
        format!("attachment; filename={}", name).into_bytes(),
    ))
}

pub async fn write_file(
    media_root: &Path,
    mut part: multer::Field<'static>,
    write_to: Option<PathBuf>,
) -> Result<SmolStr, ServerError> {
    let id = gen_rand_inline_str();
    let path = write_to.unwrap_or_else(|| media_root.join(id.as_str()));
    if path.exists() {
        return Ok(id);
    }
    let first_chunk = part.chunk().await?.ok_or(ServerError::MissingFiles)?;

    let file = tokio::fs::OpenOptions::default()
        .append(true)
        .create(true)
        .open(path)
        .await?;
    let mut buf_writer = BufWriter::new(file);

    // [tag:ascii_filename_upload]
    let name = part.file_name().unwrap_or("unknown");
    // [tag:ascii_mimetype_upload]
    let content_type = part
        .content_type()
        .map(|m| m.essence_str())
        .or_else(|| infer::get(&first_chunk).map(|t| t.mime_type()))
        .unwrap_or("application/octet-stream");

    // Write prefix
    buf_writer.write_all(name.as_bytes()).await?;
    buf_writer.write_all(&[SEPERATOR]).await?;
    buf_writer.write_all(content_type.as_bytes()).await?;
    buf_writer.write_all(&[SEPERATOR]).await?;

    // Write our first chunk
    buf_writer.write_all(&first_chunk).await?;

    // flush before starting to write other chunks
    buf_writer.flush().await?;

    while let Some(chunk) = part.chunk().await? {
        buf_writer.write_all(&chunk).await?;
    }

    // flush everything else
    buf_writer.flush().await?;

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
) -> Result<(HeaderValue, HeaderValue, hyper::Body, HeaderValue), ServerError> {
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
        hyper::Body::wrap_stream(file_stream(file, buf_size, (start, end))),
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
