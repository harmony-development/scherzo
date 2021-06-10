use crate::{
    impls::{auth::SessionMap, gen_rand_arr, rate},
    ServerError,
};

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use futures_util::StreamExt;
use harmony_rust_sdk::api::{
    exports::{
        hrpc::{futures_util, server::ServerError as HrpcError, warp},
        prost::bytes::Buf,
    },
    rest::{extract_file_info_from_download_response, FileId},
};
use reqwest::StatusCode;
use tracing::info;
use warp::{filters::multipart::*, filters::BoxedFilter, reply::Response, Filter, Reply};

const SEPERATOR: u8 = b'\n';

pub struct RestConfig {
    pub media_root: Arc<PathBuf>,
    pub sessions: SessionMap,
    pub max_length: u64,
}

pub fn rest(data: RestConfig) -> BoxedFilter<(impl Reply,)> {
    download(data.media_root.clone())
        .or(upload(data.sessions, data.media_root, data.max_length))
        .boxed()
}

pub fn download(media_root: Arc<PathBuf>) -> BoxedFilter<(impl Reply,)> {
    let http_client = reqwest::Client::new();
    warp::get()
        .and(warp::path("_harmony"))
        .and(warp::path("media"))
        .and(warp::path("download"))
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(rate(10, 5))
        .and_then(move |id: String| {
            let id = urlencoding::decode(&id).unwrap_or(id);
            let media_root = media_root.clone();
            let http_client = http_client.clone();
            async move {
                let file_id =
                    FileId::from_str(&id).map_err(|_| reject(ServerError::InvalidFileId))?;
                let reqwest_or_404 = |err: reqwest::Error| {
                    if err.status().unwrap() == StatusCode::NOT_FOUND {
                        reject(ServerError::MediaNotFound)
                    } else {
                        reject(err)
                    }
                };
                match file_id {
                    FileId::External(url) => {
                        info!("Serving external image from {}", url);
                        let resp = http_client
                            .get(url)
                            .send()
                            .await
                            .map_err(reject)?
                            .error_for_status()
                            .map_err(reqwest_or_404)?;
                        let filename = resp
                            .url()
                            .path_segments()
                            .expect("cannot be a cannot-be-a-base url")
                            .last()
                            .unwrap_or("unknown")
                            .to_string();
                        let data = resp.bytes().await.map_err(reject)?;
                        if let Some(content_type) = infer::get(&data)
                            .map(|t| {
                                t.mime_type()
                                    .starts_with("image")
                                    .then(|| t.mime_type().to_string())
                            })
                            .flatten()
                        {
                            Ok((filename, content_type, data.to_vec()))
                        } else {
                            Err(reject(ServerError::NotAnImage))
                        }
                    }
                    FileId::Hmc(hmc) => {
                        info!("Serving HMC from {}", hmc);
                        let resp = http_client
                            .get(format!(
                                "https://{}:{}/_harmony/media/download/{}",
                                hmc.server(),
                                hmc.port(),
                                hmc.id()
                            ))
                            .send()
                            .await
                            .map_err(reject)?
                            .error_for_status()
                            .map_err(reqwest_or_404)?;
                        let (name, mimetype, _) =
                            extract_file_info_from_download_response(resp.headers())
                                .map_err(|e| reject(ServerError::Unexpected(e.to_string())))?;
                        let data = resp.bytes().await.map_err(reject)?;
                        Ok((name, mimetype, data.to_vec()))
                    }
                    FileId::Id(id) => {
                        info!("Serving local media with id {}", id);
                        get_file(media_root.as_ref(), &id).await.map_err(reject)
                    }
                }
            }
        })
        .map(
            |(file_name, content_type, data): (String, String, Vec<u8>)| {
                let mut resp = Response::new(data.into());
                resp.headers_mut()
                    .insert("Content-Type", content_type.parse().unwrap());
                // TODO: content disposition attachment thingy?
                resp.headers_mut().insert(
                    "Content-Disposition",
                    format!("attachment; filename={}", file_name)
                        .parse()
                        .unwrap(),
                );
                resp
            },
        )
        .boxed()
}

pub fn upload(
    sessions: SessionMap,
    media_root: Arc<PathBuf>,
    max_length: u64,
) -> BoxedFilter<(impl Reply,)> {
    warp::post()
        .and(warp::path("_harmony"))
        .and(warp::path("media"))
        .and(warp::path("upload"))
        .and(warp::path::end())
        .and(rate(5, 5))
        .and(
            warp::filters::header::header("Authorization").and_then(move |token: String| {
                let res = if !sessions.contains_key(token.as_str()) {
                    Err(reject(ServerError::Unauthenticated))
                } else {
                    Ok(())
                };
                async move { res }
            }),
        )
        .untuple_one()
        .and(warp::query::<HashMap<String, String>>())
        .and(form().max_length(max_length))
        .and_then(move |param: HashMap<String, String>, mut form: FormData| {
            let media_root = media_root.clone();
            async move {
                if let Some(res) = form.next().await {
                    let mut part = res.map_err(reject)?;
                    let data = part
                        .data()
                        .await
                        .ok_or_else(|| reject(ServerError::MissingFiles))?
                        .map_err(reject)?;
                    let name = param.get("filename").map_or("unknown", |a| a.as_str());
                    let content_type = param
                        .get("contentType")
                        .map_or("application/octet-stream", |a| a.as_str());
                    let id_arr = gen_rand_arr::<64>();
                    // Safety: gen_rand_arr only generates alphanumerics, so it will always be a valid str [ref:alphanumeric_array_gen]
                    let id = unsafe { std::str::from_utf8_unchecked(&id_arr) };
                    let data = [
                        name.as_bytes(),
                        &[SEPERATOR],
                        content_type.as_bytes(),
                        &[SEPERATOR],
                        data.chunk(),
                    ]
                    .concat();

                    tokio::fs::write(media_root.join(id), data)
                        .await
                        .map_err(reject)?;

                    Ok(format!("{{ \"id\": \"{}\" }}", id))
                } else {
                    Err(reject(ServerError::MissingFiles))
                }
            }
        })
        .boxed()
}

#[inline(always)]
fn reject(err: impl Into<ServerError>) -> warp::Rejection {
    warp::reject::custom(HrpcError::Custom(err.into()))
}

async fn get_file(media_root: &Path, id: &str) -> Result<(String, String, Vec<u8>), ServerError> {
    match tokio::fs::read(media_root.join(id)).await {
        Ok(mut raw) => {
            let mut pos = raw.iter().enumerate().filter(|(_, b)| **b == SEPERATOR);
            let filename_sep = pos.next().unwrap().0;
            let mimetype_sep = pos.next().unwrap().0 - filename_sep - 1;
            drop(pos);

            let filename = raw
                .drain(0..=filename_sep)
                .take(filename_sep)
                .map(char::from)
                .collect::<String>();
            let mimetype = raw
                .drain(0..=mimetype_sep)
                .take(mimetype_sep)
                .map(char::from)
                .collect::<String>();

            Ok((filename, mimetype, raw))
        }
        Err(err) => {
            if let std::io::ErrorKind::NotFound = err.kind() {
                Err(ServerError::MediaNotFound)
            } else {
                Err(err.into())
            }
        }
    }
}
