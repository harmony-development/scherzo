use super::{auth::SessionMap, gen_rand_str, rate};
use crate::ServerError;

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
use warp::{filters::multipart::*, filters::BoxedFilter, reply::Response, Filter, Reply};

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
    warp::path("_harmony")
        .and(warp::path("media"))
        .and(warp::path("download"))
        .and(warp::path::end())
        .and(rate(10, 5))
        .and(warp::path::param::<String>())
        .and_then(move |id: String| {
            let id = urlencoding::decode(&id).unwrap_or(id);
            let media_root = media_root.clone();
            let http_client = http_client.clone();
            let map_reqwest_err = |err| reject(ServerError::ReqwestError(err));
            async move {
                let file_id =
                    FileId::from_str(&id).map_err(|_| reject(ServerError::InvalidFileId))?;
                match file_id {
                    FileId::External(url) => {
                        let resp = http_client
                            .get(url)
                            .send()
                            .await
                            .map_err(map_reqwest_err)?
                            .error_for_status()
                            .map_err(map_reqwest_err)?;
                        let filename = resp
                            .url()
                            .path_segments()
                            .expect("cannot be a cannot-be-a-base url")
                            .last()
                            .unwrap_or("unknown")
                            .to_string();
                        let data = resp.bytes().await.map_err(map_reqwest_err)?;
                        let content_type = infer::get(&data).map_or_else(
                            || "application/octet-stream".to_string(),
                            |k| k.mime_type().to_string(),
                        );
                        Ok((filename, content_type, data.to_vec()))
                    }
                    FileId::Hmc(hmc) => {
                        let resp = http_client
                            .get(format!(
                                "https://{}:{}/_harmony/media/download/{}",
                                hmc.server(),
                                hmc.port(),
                                hmc.id()
                            ))
                            .send()
                            .await
                            .map_err(map_reqwest_err)?
                            .error_for_status()
                            .map_err(map_reqwest_err)?;
                        let (name, mimetype, _) =
                            extract_file_info_from_download_response(resp.headers())
                                .map_err(|e| reject(ServerError::Unexpected(e.to_string())))?;
                        let data = resp.bytes().await.map_err(map_reqwest_err)?;
                        Ok((name, mimetype, data.to_vec()))
                    }
                    FileId::Id(_) => get_file(media_root.as_ref(), &id)
                        .await
                        .map_err(|err| reject(ServerError::IoError(err))),
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
            warp::filters::header::header("Authorization").map(move |token: String| {
                if !sessions.contains_key(&token) {
                    Err(reject(ServerError::Unauthenticated))
                } else {
                    Ok(())
                }
            }),
        )
        .and(warp::query::<HashMap<String, String>>())
        .and(form().max_length(max_length))
        .and_then(
            move |_, param: HashMap<String, String>, mut form: FormData| {
                let media_root = media_root.clone();
                async move {
                    if let Some(res) = form.next().await {
                        let mut part = res.map_err(|err| reject(ServerError::WarpError(err)))?;
                        let data = part
                            .data()
                            .await
                            .ok_or_else(|| reject(ServerError::MissingFiles))?
                            .map_err(|err| reject(ServerError::WarpError(err)))?;
                        let name = param.get("filename").map_or("unknown", |a| a.as_str());
                        let content_type = param
                            .get("contentType")
                            .map_or("application/octet-stream", |a| a.as_str());
                        // TODO: check if id already exists (though will be expensive)
                        let id = gen_rand_str(64);
                        let filename = format!(
                            "{}#{}#{}",
                            id,
                            urlencoding::encode(name),
                            urlencoding::encode(content_type)
                        );

                        tokio::fs::write(media_root.clone().join(filename), data.chunk())
                            .await
                            .map_err(|err| reject(ServerError::IoError(err)))?;

                        Ok(format!("{{ \"id\": \"{}\" }}", id))
                    } else {
                        Err(reject(ServerError::MissingFiles))
                    }
                }
            },
        )
        .boxed()
}

#[inline(always)]
fn reject(err: ServerError) -> warp::Rejection {
    warp::reject::custom(HrpcError::Custom(err))
}

async fn get_file(media_root: &Path, id: &str) -> std::io::Result<(String, String, Vec<u8>)> {
    let mut dir = tokio::fs::read_dir(media_root).await?;
    while let Some(entry) = dir.next_entry().await? {
        if entry
            .file_name()
            .to_str()
            .expect("all media names must be utf-8")
            .starts_with(id)
        {
            let entry_name = entry.file_name();
            let mut split = entry_name
                .to_str()
                .expect("all media names must be utf-8")
                .split('#');
            split.next();
            let file_name = split.next().unwrap();
            let content_type = split.next().unwrap();
            return Ok((
                file_name.to_string(),
                content_type.to_string(),
                tokio::fs::read(entry.path()).await?,
            ));
        }
    }
    unreachable!("media root must have id")
}
