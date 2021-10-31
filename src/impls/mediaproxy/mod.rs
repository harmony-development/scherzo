use ahash::RandomState;
use dashmap::{mapref::one::Ref, DashMap};
use harmony_rust_sdk::api::{
    exports::hrpc::client::http_client,
    mediaproxy::{fetch_link_metadata_response::Data, *},
};
use hyper::{body::Buf, StatusCode, Uri};
use webpage::HTML;

use std::time::Instant;

use super::{get_mimetype, http, prelude::*, HttpClient};

pub mod can_instant_view;
pub mod fetch_link_metadata;
pub mod instant_view;

fn site_metadata_from_html(html: &HTML) -> SiteMetadata {
    SiteMetadata {
        page_title: html.title.clone().unwrap_or_default(),
        description: html.description.clone().unwrap_or_default(),
        url: html.url.clone().unwrap_or_default(),
        image: html
            .opengraph
            .images
            .last()
            .map(|og| og.url.clone())
            .unwrap_or_default(),
        ..Default::default()
    }
}

#[derive(Clone)]
enum Metadata {
    Site(Arc<HTML>),
    Media {
        filename: SmolStr,
        mimetype: SmolStr,
    },
}

impl From<Metadata> for Data {
    fn from(metadata: Metadata) -> Self {
        match metadata {
            Metadata::Site(html) => Data::IsSite(site_metadata_from_html(&html)),
            Metadata::Media { filename, mimetype } => Data::IsMedia(MediaMetadata {
                mimetype: mimetype.into(),
                filename: filename.into(),
            }),
        }
    }
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

#[derive(Clone)]
pub struct MediaproxyServer {
    http: HttpClient,
    disable_ratelimits: bool,
    deps: Arc<Dependencies>,
}

impl MediaproxyServer {
    pub fn new(deps: Arc<Dependencies>) -> Self {
        Self {
            http: http_client(),
            disable_ratelimits: deps.config.policy.disable_ratelimits,
            deps,
        }
    }

    async fn fetch_metadata(&self, raw_url: String) -> Result<Metadata, ServerError> {
        // Get from cache if available
        if let Some(value) = get_from_cache(&raw_url) {
            return Ok(value.value.clone());
        }

        let url: Uri = raw_url.parse().map_err(ServerError::InvalidUrl)?;
        let response = self.http.get(url.clone()).await?;
        if !response.status().is_success() {
            let err = if response.status() == StatusCode::NOT_FOUND {
                ServerError::LinkNotFound(url)
            } else {
                // TODO: change to proper error
                ServerError::InternalServerError
            };
            return Err(err);
        }

        let is_html = response
            .headers()
            .get(&http::header::CONTENT_TYPE)
            .and_then(|v| v.as_bytes().get(0..9))
            .map_or(false, |v| v.eq_ignore_ascii_case(b"text/html"));

        let metadata = if is_html {
            let body = hyper::body::aggregate(response.into_body()).await?;
            let html = String::from_utf8_lossy(body.chunk());
            let html = webpage::HTML::from_string(html.into(), Some(raw_url))?;
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
                .or_else(|| url.path().split('/').last().filter(|n| !n.is_empty()))
                .unwrap_or("unknown")
                .into();
            Metadata::Media {
                filename,
                mimetype: get_mimetype(&response).into(),
            }
        };

        // Insert to cache since successful
        CACHE.insert(
            url.to_string(),
            TimedCacheValue {
                value: metadata.clone(),
                since: Instant::now(),
            },
        );

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
