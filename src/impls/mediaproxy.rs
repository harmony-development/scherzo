use reqwest::{Client, Response, Url};
use webpage::HTML;

use crate::{
    impls::auth::{self, SessionMap},
    ServerError,
};

use harmony_rust_sdk::api::{
    exports::hrpc::Request,
    mediaproxy::{fetch_link_metadata_response::Data, *},
};

#[derive(Debug)]
enum Metadata {
    Site(HTML),
    Media { filename: String, mimetype: String },
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

    fn auth<T>(&self, request: &Request<T>) -> Result<u64, ServerError> {
        auth::check_auth(&self.valid_sessions, request)
    }

    async fn fetch_metadata(&self, url: String) -> Result<Metadata, ServerError> {
        let url: Url = url.parse().map_err(ServerError::InvalidUrl)?;
        let response = self.http.get(url.as_ref()).send().await?;

        let mimetype = self.get_mimetype(&response);

        Ok(if mimetype == "text/html" {
            let html = response.text().await?;
            let html = webpage::HTML::from_string(html, Some(url.into()))?;
            Metadata::Site(html)
        } else {
            let filename = response
                .headers()
                .get("Content-Disposition")
                .map(|val| val.to_str().ok())
                .flatten()
                .or_else(|| Some(url.path_segments()?.last()).flatten())
                .unwrap_or("unknown")
                .to_string();
            Metadata::Media {
                filename,
                mimetype: mimetype.to_string(),
            }
        })
    }

    fn get_mimetype<'a>(&self, response: &'a Response) -> &'a str {
        response
            .headers()
            .get("Content-Type")
            .map(|val| val.to_str().ok())
            .flatten()
            .unwrap_or("application/octet-stream")
    }
}

#[harmony_rust_sdk::api::exports::hrpc::async_trait]
impl media_proxy_service_server::MediaProxyService for MediaproxyServer {
    type Error = ServerError;

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
            Metadata::Media { filename, mimetype } => {
                Data::IsMedia(MediaMetadata { mimetype, filename })
            }
        };

        Ok(FetchLinkMetadataResponse { data: Some(data) })
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

    async fn can_instant_view(
        &self,
        request: Request<InstantViewRequest>,
    ) -> Result<CanInstantViewResponse, Self::Error> {
        self.auth(&request)?;

        let InstantViewRequest { url } = request.into_parts().0;

        let url: Url = url.parse().map_err(ServerError::InvalidUrl)?;
        let response = self.http.get(url).send().await?;

        let ok = self.get_mimetype(&response).eq("text/html");

        Ok(CanInstantViewResponse {
            can_instant_view: ok,
        })
    }
}
