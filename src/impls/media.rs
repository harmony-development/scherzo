use std::{
    fs::Metadata,
    path::{Path, PathBuf},
};

use hrpc::exports::futures_util::{Stream, StreamExt};
use prost::bytes::Bytes;
use smol_str::SmolStr;
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
};

use crate::{config::MediaConfig, utils::gen_rand_inline_str, ServerError};

const SEPERATOR: u8 = b'\n';

pub struct FileHandle {
    pub file: File,
    pub metadata: Metadata,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub range: (u64, u64),
}

impl FileHandle {
    pub async fn read(mut self) -> Result<Vec<u8>, ServerError> {
        let mut raw = Vec::with_capacity(self.size as usize);
        self.file.read_to_end(&mut raw).await?;
        Ok(raw)
    }
}

pub struct MediaStore {
    root: PathBuf,
}

impl MediaStore {
    pub fn new(config: &MediaConfig) -> Self {
        Self {
            root: config.media_root.clone(),
        }
    }

    pub async fn write_file(
        &self,
        mut chunks: impl Stream<Item = Result<Bytes, ServerError>> + Unpin,
        name: &str,
        mime: &str,
    ) -> Result<SmolStr, ServerError> {
        let id = gen_rand_inline_str();
        let path = self.root.join(id.as_str());
        if path.exists() {
            return Ok(id);
        }
        let first_chunk = chunks.next().await.ok_or(ServerError::MissingFiles)??;

        let file = tokio::fs::OpenOptions::default()
            .append(true)
            .create(true)
            .open(path)
            .await?;
        let mut buf_writer = BufWriter::new(file);

        // Write prefix
        buf_writer.write_all(name.as_bytes()).await?;
        buf_writer.write_all(&[SEPERATOR]).await?;
        buf_writer.write_all(mime.as_bytes()).await?;
        buf_writer.write_all(&[SEPERATOR]).await?;

        // Write our first chunk
        buf_writer.write_all(&first_chunk).await?;

        // flush before starting to write other chunks
        buf_writer.flush().await?;

        while let Some(chunk) = chunks.next().await.transpose()? {
            buf_writer.write_all(&chunk).await?;
        }

        // flush everything else
        buf_writer.flush().await?;

        Ok(id)
    }

    pub async fn get_file(&self, id: &str) -> Result<FileHandle, ServerError> {
        let is_jpeg = is_id_jpeg(id);
        let (mut file, metadata, file_path) = self.get_file_handle(id).await?;
        let (filename_raw, mimetype_raw, _) = read_bufs(&file_path, &mut file, is_jpeg).await?;

        let (start, end) = calculate_range(&filename_raw, &mimetype_raw, &metadata, is_jpeg);

        let mime = String::from_utf8_lossy(&mimetype_raw).into_owned();
        let name = String::from_utf8_lossy(&filename_raw).into_owned();

        Ok(FileHandle {
            file,
            metadata,
            mime,
            name,
            size: end - start,
            range: (start, end),
        })
    }

    async fn get_file_handle(&self, id: &str) -> Result<(File, Metadata, PathBuf), ServerError> {
        let file_path = self.root.join(id);
        let file = tokio::fs::File::open(&file_path).await.map_err(|err| {
            if let std::io::ErrorKind::NotFound = err.kind() {
                ServerError::MediaNotFound
            } else {
                err.into()
            }
        })?;
        let metadata = file.metadata().await?;
        Ok((file, metadata, file_path))
    }
}

pub async fn read_bufs<'file>(
    path: &Path,
    file: &'file mut File,
    is_jpeg: bool,
) -> Result<(Vec<u8>, Vec<u8>, BufReader<&'file mut File>), ServerError> {
    if is_jpeg {
        let mimetype = infer::get_from_path(path).ok().flatten().map_or_else(
            || b"image/jpeg".to_vec(),
            |t| t.mime_type().as_bytes().to_vec(),
        );
        Ok((b"unknown".to_vec(), mimetype, BufReader::new(file)))
    } else {
        let mut buf_reader = BufReader::new(file);

        let mut filename_raw = Vec::with_capacity(20);
        buf_reader.read_until(SEPERATOR, &mut filename_raw).await?;
        filename_raw.pop();

        let mut mimetype_raw = Vec::with_capacity(20);
        buf_reader.read_until(SEPERATOR, &mut mimetype_raw).await?;
        mimetype_raw.pop();

        Ok((filename_raw, mimetype_raw, buf_reader))
    }
}

pub fn calculate_range(
    filename_raw: &[u8],
    mimetype_raw: &[u8],
    metadata: &Metadata,
    is_jpeg: bool,
) -> (u64, u64) {
    // + 2 is because we need to factor in the 2 b'\n' seperators
    let start = is_jpeg
        .then(|| 0)
        .unwrap_or((filename_raw.len() + mimetype_raw.len()) as u64 + 2);
    let end = metadata.len();

    (start, end)
}

pub fn is_id_jpeg(id: &str) -> bool {
    id.ends_with("_jpeg") || id.ends_with("_jpegthumb")
}
