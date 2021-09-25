use super::prelude::*;

use harmony_rust_sdk::api::voice::{voice_service_server::VoiceService, *};

pub struct VoiceServer {}

impl VoiceServer {
    pub fn new(_deps: &Dependencies) -> Self {
        Self {}
    }
}

#[async_trait]
impl VoiceService for VoiceServer {
    type Error = ServerError;

    async fn connect(
        &self,
        _request: Request<ConnectRequest>,
    ) -> Result<ConnectResponse, HrpcServerError<Self::Error>> {
        Err(ServerError::NotImplemented.into())
    }

    type StreamStateValidationType = ();

    async fn stream_state(
        &self,
        _validation_value: Self::StreamStateValidationType,
        _socket: Socket<StreamStateRequest, StreamStateResponse>,
    ) {
    }

    async fn stream_state_validation(
        &self,
        _request: Request<Option<StreamStateRequest>>,
    ) -> Result<Self::StreamStateValidationType, HrpcServerError<Self::Error>> {
        Err(ServerError::NotImplemented.into())
    }
}
