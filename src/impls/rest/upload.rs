use harmony_rust_sdk::api::exports::hrpc::server::handler::Handler;

use super::*;

pub fn handler(deps: Arc<Dependencies>) -> Handler {
    let upload = service_fn(move |request: HttpRequest| {
        let deps = deps.clone();

        let auth_res = deps.valid_sessions.auth_header_map(request.headers());

        async move {
            if let Err(err) = auth_res {
                return Ok(err.as_error_response());
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
            let boundary = bail_result_as_response!(boundary_res);
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
                        let id = bail_result_as_response!(
                            write_file(deps.config.media.media_root.as_path(), field, None).await
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
    Handler::new(
        ServiceBuilder::new()
            .rate_limit(3, Duration::from_secs(5))
            .service(upload),
    )
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
