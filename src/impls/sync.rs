use super::*;

use harmony_rust_sdk::api::{exports::hrpc::async_trait, sync::*};

pub struct SyncServer;

#[async_trait]
impl postbox_service_server::PostboxService for SyncServer {
    type Error = ServerError;

    type PullValidationType = ();

    async fn pull_validation(
        &self,
        request: harmony_rust_sdk::api::exports::hrpc::Request<Option<v1::Ack>>,
    ) -> Result<Self::PullValidationType, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn pull(
        &self,
        validation_value: Self::PullValidationType,
        socket: harmony_rust_sdk::api::exports::hrpc::server::Socket<v1::Ack, v1::Syn>,
    ) {
        todo!()
    }

    async fn push(
        &self,
        request: harmony_rust_sdk::api::exports::hrpc::Request<v1::Event>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }
}
