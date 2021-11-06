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
            bail_result_as_response,
            body::{box_body, full_box_body},
            client::HttpClient,
            exports::futures_util::{
                future::{self, Either},
                ready, stream, FutureExt, Stream, StreamExt,
            },
            server::{prelude::CustomError, router::Routes, MakeRoutes},
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

pub mod about;
pub mod download;
pub mod upload;

const SEPERATOR: u8 = b'\n';

pub struct RestServer {
    deps: Arc<Dependencies>,
}

impl RestServer {
    pub fn new(deps: Arc<Dependencies>) -> Self {
        Self { deps }
    }
}

impl MakeRoutes for RestServer {
    fn make_routes(&self) -> Routes {
        let deps = self.deps.clone();
        let download = download::handler(deps);

        let deps = self.deps.clone();
        let upload = upload::handler(deps);

        let deps = self.deps.clone();
        let about = about::handler(deps);

        Routes::new()
            .route("/_harmony/media/download/:id", download)
            .route("/_harmony/media/upload", upload)
            .route("/_harmony/about", about)
    }
}
