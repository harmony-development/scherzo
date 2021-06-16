use ahash::RandomState;
use dashmap::{mapref::one::Ref, DashMap};
use reqwest::{Client, Response, Url};
use smol_str::SmolStr;
use webpage::HTML;

use std::time::Instant;

use crate::{
    impls::{
        auth::{self, SessionMap},
        rate,
    },
    ServerError,
};

use harmony_rust_sdk::api::{
    exports::hrpc::{warp::filters::BoxedFilter, Request},
    mediaproxy::{fetch_link_metadata_response::Data, *},
};

#[derive(Debug, Clone)]
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

fn get_mimetype(response: &Response) -> &str {
    response
        .headers()
        .get("Content-Type")
        .map(|val| val.to_str().ok())
        .flatten()
        .unwrap_or("application/octet-stream")
}

#[derive(Debug)]
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

    #[inline(always)]
    fn auth<T>(&self, request: &Request<T>) -> Result<u64, ServerError> {
        auth::check_auth(&self.valid_sessions, request)
    }

    async fn fetch_metadata(&self, raw_url: String) -> Result<Metadata, ServerError> {
        // Get from cache if available
        if let Some(value) = get_from_cache(&raw_url) {
            return Ok(value.value.clone());
        }

        let url: Url = raw_url.parse().map_err(ServerError::InvalidUrl)?;
        let response = self.http.get(url.as_ref()).send().await?;

        let mimetype = get_mimetype(&response);

        let metadata = if mimetype == "text/html" {
            let html = response.text().await?;
            let html = webpage::HTML::from_string(html, Some(raw_url))?;
            Metadata::Site(html)
        } else {
            let filename = response
                .headers()
                .get("Content-Disposition")
                .map(|val| val.to_str().ok())
                .flatten()
                .or_else(|| Some(url.path_segments()?.last()).flatten())
                .unwrap_or("unknown")
                .into();
            Metadata::Media {
                filename,
                mimetype: mimetype.into(),
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

    fn fetch_link_metadata_pre(&self) -> BoxedFilter<()> {
        rate(2, 1)
    }

    async fn fetch_link_metadata(
        &self,
        request: Request<FetchLinkMetadataRequest>,
    ) -> Result<FetchLinkMetadataResponse, Self::Error> {
        self.auth(&request)?;

        let FetchLinkMetadataRequest { url } = request.into_parts().0;

        let data = match self.fetch_metadata(url).await? {
            Metadata::Site(html) => Data::IsSite(SiteMetadata {
                page_title: html.title.unwrap_or_default(),
                description: html.description.unwrap_or_default(),
                url: html.url.unwrap_or_default(),
                ..Default::default()
            }),
            Metadata::Media { filename, mimetype } => Data::IsMedia(MediaMetadata {
                mimetype: mimetype.into(),
                filename: filename.into(),
            }),
        };

        Ok(FetchLinkMetadataResponse { data: Some(data) })
    }

    fn instant_view_pre(&self) -> BoxedFilter<()> {
        rate(1, 5)
    }

    async fn instant_view(
        &self,
        request: Request<InstantViewRequest>,
    ) -> Result<InstantViewResponse, Self::Error> {
        self.auth(&request)?;

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

    fn can_instant_view_pre(&self) -> BoxedFilter<()> {
        rate(20, 5)
    }

    async fn can_instant_view(
        &self,
        request: Request<InstantViewRequest>,
    ) -> Result<CanInstantViewResponse, Self::Error> {
        self.auth(&request)?;

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