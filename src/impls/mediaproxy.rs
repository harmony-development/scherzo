use ahash::RandomState;
use dashmap::{mapref::one::Ref, DashMap};
use reqwest::{Client, StatusCode, Url};
use scherzo_derive::*;
use smol_str::SmolStr;
use webpage::HTML;

use std::time::Instant;

use crate::{
    impls::{auth::SessionMap, get_mimetype, http},
    ServerError,
};

use harmony_rust_sdk::api::{
    exports::hrpc::Request,
    mediaproxy::{fetch_link_metadata_response::Data, *},
};

#[derive(Clone)]
enum Metadata {
    Site(HTML),
    Media {
        filename: SmolStr,
        mimetype: SmolStr,
    },
}

struct TimedCacheValue<T> {
    value: T,
    since: Instant,
}

// TODO: investigate possible optimization since the key will always be an URL?
lazy_static::lazy_static! {
    static ref CACHE: DashMap<String, TimedCacheValue<Metadata>, RandomState> = DashMap::with_capacity_and_hasher(512, RandomState::new());
}

fn get_from_cache(url: &str) -> Option<Ref<'_, String, TimedCacheValue<Metadata>, RandomState>> {
    match CACHE.get(url) {
        // Value is available, check if it is expired
        Some(val) => {
            // Remove value if it is expired
            if val.since.elapsed().as_secs() >= 30 * 60 {
                drop(val); // explicit drop to tell we don't need it anymore
                CACHE.remove(url);
                None
            } else {
                Some(val)
            }
        }
        // No value available
        None => None,
    }
}

pub struct MediaproxyServer {
    http: Client,
    valid_sessions: SessionMap,
}

impl MediaproxyServer {
    pub fn new(valid_sessions: SessionMap) -> Self {
        Self {
            http: Client::new(),
            valid_sessions,
        }
    }

    async fn fetch_metadata(&self, raw_url: String) -> Result<Metadata, ServerError> {
        // Get from cache if available
        if let Some(value) = get_from_cache(&raw_url) {
            return Ok(value.value.clone());
        }

        let url: Url = raw_url.parse().map_err(ServerError::InvalidUrl)?;
        let response = match self.http.get(url.as_ref()).send().await?.error_for_status() {
            Ok(response) => response,
            Err(err) => {
                let err = if err.status() == Some(StatusCode::NOT_FOUND) {
                    ServerError::LinkNotFound(url)
                } else {
                    ServerError::ReqwestError(err)
                };
                return Err(err);
            }
        };

        let is_html = response
            .headers()
            .get(&http::header::CONTENT_TYPE)
            .and_then(|v| v.as_bytes().get(0..9))
            .map_or(false, |v| v.eq_ignore_ascii_case(b"text/html"));

        let metadata = if is_html {
            let html = response.text().await?;
            let html = webpage::HTML::from_string(html, Some(raw_url))?;
            Metadata::Site(html)
        } else {
            let filename = response
                .headers()
                .get(&http::header::CONTENT_DISPOSITION)
                .and_then(|val| val.to_str().ok())
                .map(|s| {
                    const FILENAME: &str = "filename=";
                    s.find(FILENAME)
                        .and_then(|f| s.get(f + FILENAME.len()..))
                        .unwrap_or(s)
                })
                .or_else(|| {
                    url.path_segments()
                        .and_then(Iterator::last)
                        .filter(|n| !n.is_empty())
                })
                .unwrap_or("unknown")
                .into();
            Metadata::Media {
                filename,
                mimetype: get_mimetype(&response).into(),
            }
        };

        // Insert to cache since successful
        CACHE.insert(
            url.into(),
            TimedCacheValue {
                value: metadata.clone(),
                since: Instant::now(),
            },
        );

        Ok(metadata)
    }
}

#[harmony_rust_sdk::api::exports::hrpc::async_trait]
impl media_proxy_service_server::MediaProxyService for MediaproxyServer {
    type Error = ServerError;

    #[rate(2, 1)]
    async fn fetch_link_metadata(
        &self,
        request: Request<FetchLinkMetadataRequest>,
    ) -> Result<FetchLinkMetadataResponse, Self::Error> {
        auth!();

        let FetchLinkMetadataRequest { url } = request.into_parts().0;

        let data = match self.fetch_metadata(url).await? {
            Metadata::Site(mut html) => Data::IsSite(SiteMetadata {
                page_title: html.title.unwrap_or_default(),
                description: html.description.unwrap_or_default(),
                url: html.url.unwrap_or_default(),
                image: html
                    .opengraph
                    .images
                    .pop()
                    .map(|og| og.url)
                    .unwrap_or_default(),
                ..Default::default()
            }),
            Metadata::Media { filename, mimetype } => Data::IsMedia(MediaMetadata {
                mimetype: mimetype.into(),
                filename: filename.into(),
            }),
        };

        Ok(FetchLinkMetadataResponse { data: Some(data) })
    }

    #[rate(1, 5)]
    async fn instant_view(
        &self,
        request: Request<InstantViewRequest>,
    ) -> Result<InstantViewResponse, Self::Error> {
        auth!();

        let InstantViewRequest { url } = request.into_parts().0;

        let data = self.fetch_metadata(url).await?;

        if let Metadata::Site(html) = data {
            let metadata = SiteMetadata {
                page_title: html.title.unwrap_or_default(),
                description: html.description.unwrap_or_default(),
                url: html.url.unwrap_or_default(),
                ..Default::default()
            };

            Ok(InstantViewResponse {
                content: html.text_content,
                is_valid: true,
                metadata: Some(metadata),
            })
        } else {
            Ok(InstantViewResponse::default())
        }
    }

    #[rate(20, 5)]
    async fn can_instant_view(
        &self,
        request: Request<InstantViewRequest>,
    ) -> Result<CanInstantViewResponse, Self::Error> {
        auth!();

        let InstantViewRequest { url } = request.into_parts().0;

        if let Some(val) = get_from_cache(&url) {
            return Ok(CanInstantViewResponse {
                can_instant_view: matches!(val.value, Metadata::Site(_)),
            });
        }

        let url: Url = url.parse().map_err(ServerError::InvalidUrl)?;
        let response = self.http.get(url).send().await?;

        let ok = get_mimetype(&response).eq("text/html");

        Ok(CanInstantViewResponse {
            can_instant_view: ok,
        })
    }
}
