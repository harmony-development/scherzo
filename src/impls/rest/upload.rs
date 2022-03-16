use std::convert::Infallible;

use hrpc::{
    exports::futures_util::{future::BoxFuture, TryStreamExt},
    server::transport::http::HttpResponse,
};
use tower::Service;

use crate::{impls::auth::get_token_from_header_map, rest_error_response};

use super::*;

pub fn handler(deps: Arc<Dependencies>) -> RateLimit<UploadService> {
    let client_ip_header_name = deps.config.policy.ratelimit.client_ip_header_name.clone();
    let allowed_ips = deps.config.policy.ratelimit.allowed_ips.clone();
    RateLimit::new(
        UploadService { deps },
        3,
        Duration::from_secs(5),
        client_ip_header_name,
        allowed_ips,
    )
}

pub struct UploadService {
    deps: Arc<Dependencies>,
}

impl Service<HttpRequest> for UploadService {
    type Response = HttpResponse;

    type Error = Infallible;

    type Future = BoxFuture<'static, Result<HttpResponse, Infallible>>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, request: HttpRequest) -> Self::Future {
        let deps = self.deps.clone();

        let fut = async move {
            if let Err(err) = deps
                .auth_with(get_token_from_header_map(request.headers()))
                .await
            {
                return Ok(err.into_rest_http_response());
            }
            let boundary_res = request
                .headers()
                .get(&header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .and_then(|v| multer::parse_boundary(v).ok());
            let boundary = match boundary_res {
                Some(b) => b,
                None => {
                    return Ok(rest_error_response(
                        "content_type header not found or was invalid".to_string(),
                        StatusCode::BAD_REQUEST,
                    ))
                }
            };
            let mut multipart = multer::Multipart::with_constraints(
                request.into_body(),
                boundary,
                multer::Constraints::new()
                    .allowed_fields(vec!["file"])
                    .size_limit(
                        multer::SizeLimit::new()
                            .whole_stream(deps.config.media.max_upload_length * 1024 * 1024),
                    ),
            );

            match multipart.next_field().await {
                Ok(maybe_field) => match maybe_field {
                    Some(field) => {
                        let name = field.file_name().unwrap_or("unknown").to_string();
                        let mime = field
                            .content_type()
                            .map_or("application/octet-stream", |m| m.essence_str())
                            .to_string();
                        let chunks = field.map_err(ServerError::MultipartError);

                        let id = match deps.media.write_file(chunks, &name, &mime).await {
                            Ok(id) => id,
                            Err(err) => return Ok(err.into_rest_http_response()),
                        };

                        Ok(http::Response::builder()
                            .status(StatusCode::OK)
                            .body(box_body(Body::from(
                                format!(r#"{{ "id": "{id}" }}"#).into_bytes(),
                            )))
                            .unwrap())
                    }
                    None => Ok(ServerError::MissingFiles.into_rest_http_response()),
                },
                Err(err) => Ok(ServerError::from(err).into_rest_http_response()),
            }
        };

        Box::pin(fut)
    }
}
