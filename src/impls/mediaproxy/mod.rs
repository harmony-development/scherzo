use crate::api::mediaproxy::{
    fetch_link_metadata_response::{metadata::Data, Metadata as HarmonyMetadata},
    *,
};
use ahash::RandomState;
use dashmap::{mapref::one::Ref, DashMap};
use reqwest::Client as HttpClient;
use webpage::HTML;

use std::{ops::Not, time::Instant};

use super::{get_mimetype, http, prelude::*};

pub mod can_instant_view;
pub mod fetch_link_metadata;
pub mod instant_view;

fn site_metadata_from_html(html: &HTML) -> SiteMetadata {
    SiteMetadata {
        page_title: html.title.clone().unwrap_or_default(),
        description: html.description.clone().unwrap_or_default(),
        url: html.url.clone().unwrap_or_default(),
        thumbnail: html
            .opengraph
            .images
            .iter()
            .map(|img| site_metadata::ThumbnailImage {
                url: img.url.clone(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

#[derive(Clone)]
enum Metadata {
    Site(Arc<HTML>),
    Media {
        filename: SmolStr,
        mimetype: SmolStr,
        size: Option<u32>,
    },
}

impl From<Metadata> for Data {
    fn from(metadata: Metadata) -> Self {
        match metadata {
            Metadata::Site(html) => Data::IsSite(site_metadata_from_html(&html)),
            Metadata::Media {
                filename,
                mimetype,
                size,
            } => Data::IsMedia(MediaMetadata {
                mimetype: mimetype.into(),
                name: filename.into(),
                size,
                ..Default::default()
            }),
        }
    }
}

impl From<Metadata> for HarmonyMetadata {
    fn from(metadata: Metadata) -> Self {
        HarmonyMetadata::new(Data::from(metadata))
    }
}

const DEFAULT_MAX_AGE: u64 = 30 * 60;

struct TimedCacheValue<T> {
    value: T,
    since: Instant,
    max_age: u64,
}

impl<T> TimedCacheValue<T> {
    fn new(value: T, max_age: u64) -> Self {
        Self {
            value,
            since: Instant::now(),
            max_age,
        }
    }
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
            if val.since.elapsed().as_secs() >= val.max_age {
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

#[derive(Clone)]
pub struct MediaproxyServer {
    http: HttpClient,
    disable_ratelimits: bool,
    deps: Arc<Dependencies>,
}

impl MediaproxyServer {
    pub fn new(deps: Arc<Dependencies>) -> Self {
        Self {
            http: HttpClient::new(),
            disable_ratelimits: deps.config.policy.ratelimit.disable,
            deps,
        }
    }

    pub fn batch(mut self) -> Self {
        self.disable_ratelimits = true;
        self
    }

    async fn fetch_metadata(&self, raw_url: String) -> Result<Metadata, ServerError> {
        // Get from cache if available
        if let Some(value) = get_from_cache(&raw_url) {
            return Ok(value.value.clone());
        }

        let response = self
            .http
            .get(&raw_url)
            .send()
            .await
            .map_err(ServerError::FailedToFetchLink)?
            .error_for_status()
            .map_err(ServerError::FailedToFetchLink)?;

        let max_age = response
            .headers()
            .get(&http::header::CACHE_CONTROL)
            .and_then(|v| {
                let s = v.to_str().ok()?;
                let parse_max_age = || {
                    s.split(',')
                        .map(str::trim)
                        .find_map(|item| {
                            item.strip_prefix("max-age=")
                                .or_else(|| item.strip_prefix("s-maxage="))
                        })
                        .and_then(|raw| raw.parse::<u64>().ok())
                };
                s.contains("no-store")
                    .not()
                    .then(parse_max_age)
                    .unwrap_or(Some(0))
            })
            .unwrap_or(DEFAULT_MAX_AGE);

        let is_html = response
            .headers()
            .get(&http::header::CONTENT_TYPE)
            .and_then(|v| v.as_bytes().get(0..9))
            .map_or(false, |v| v.eq_ignore_ascii_case(b"text/html"));

        let metadata = if is_html {
            let body = response
                .bytes()
                .await
                .map_err(ServerError::FailedToFetchLink)?;
            let html = String::from_utf8_lossy(&body);
            let html = webpage::HTML::from_string(html.into(), Some(raw_url.clone()))?;
            Metadata::Site(Arc::new(html))
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
                .or_else(|| raw_url.split('/').last().filter(|n| !n.is_empty()))
                .unwrap_or("unknown")
                .into();
            let size = get_content_length(response.headers())
                .to_str()
                .ok()
                .and_then(|size| size.parse::<u32>().ok());
            Metadata::Media {
                filename,
                mimetype: get_mimetype(response.headers()).into(),
                size,
            }
        };

        // Insert to cache since successful
        CACHE.insert(raw_url, TimedCacheValue::new(metadata.clone(), max_age));

        Ok(metadata)
    }
}

impl media_proxy_service_server::MediaProxyService for MediaproxyServer {
    impl_unary_handlers! {
        #[rate(2, 1)]
        fetch_link_metadata, FetchLinkMetadataRequest, FetchLinkMetadataResponse;
        #[rate(1, 5)]
        instant_view, InstantViewRequest, InstantViewResponse;
        #[rate(20, 5)]
        can_instant_view, CanInstantViewRequest, CanInstantViewResponse;
    }
}
