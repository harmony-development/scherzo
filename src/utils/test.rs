use harmony_rust_sdk::api::{
    auth::auth_service_client::AuthServiceClient, chat::chat_service_client::ChatServiceClient,
    emote::emote_service_client::EmoteServiceClient,
    mediaproxy::media_proxy_service_client::MediaProxyServiceClient,
    profile::profile_service_client::ProfileServiceClient,
};
use hrpc::client::transport::mock::Mock as MockClient;

pub struct Clients {
    pub chat: ChatServiceClient<MockClient>,
    pub auth: AuthServiceClient<MockClient>,
    pub mediaproxy: MediaProxyServiceClient<MockClient>,
    pub profile: ProfileServiceClient<MockClient>,
    pub emote: EmoteServiceClient<MockClient>,
}

pub fn create_mock(
    deps: triomphe::Arc<crate::impls::Dependencies>,
    fed: tokio::sync::mpsc::UnboundedReceiver<crate::impls::sync::EventDispatch>,
) -> Clients {
    use hrpc::{
        common::transport::mock::new_mock_channels,
        server::transport::{mock::Mock as MockServer, Transport},
    };

    let (tx, rx) = new_mock_channels();

    let transport = MockServer::new(rx);
    let server = crate::impls::setup_server(deps, fed, tracing::Level::DEBUG);
    let fut = transport.serve(server.0);

    tokio::spawn(fut);

    let transport = MockClient::new(tx);

    let chat = ChatServiceClient::new_transport(transport.clone());
    let auth = AuthServiceClient::new_transport(transport.clone());
    let mediaproxy = MediaProxyServiceClient::new_transport(transport.clone());
    let profile = ProfileServiceClient::new_transport(transport.clone());
    let emote = EmoteServiceClient::new_transport(transport);

    Clients {
        chat,
        auth,
        emote,
        mediaproxy,
        profile,
    }
}

macro_rules! unit_test {
    ($name:ident, $clients:ident, $body:expr) => {
        #[cfg(test)]
        #[tokio::test]
        async fn $name() {
            let mut config = $crate::config::Config::default();
            config.policy.ratelimit.disable = true;
            let db = $crate::db::open_temp();
            let (deps, fed) = $crate::impls::Dependencies::new(&db, config)
                .await
                .expect("failed to create dependencies");

            let mut $clients = $crate::utils::test::create_mock(deps, fed);

            {
                $body
            }
        }
    };
}

pub(crate) use unit_test;
