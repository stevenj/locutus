use locutus_node::either::Either;

use locutus_node::*;
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::sync::mpsc;
use warp::hyper::body::Bytes;
use warp::ws::WebSocket;
use warp::{filters::BoxedFilter, reply, Filter, Rejection, Reply};

use crate::{
    errors,
    state_handling::{get_state, state_changes_notification, update_state},
    ClientHandlingMessage, HostResult,
};

const PARALLELISM: usize = 10; // TODO: get this from config, or whatever optimal way

pub struct HttpGateway {
    server_request: mpsc::Receiver<ClientHandlingMessage>,
    response_channels: HashMap<ClientId, mpsc::UnboundedSender<HostResult>>,
}

impl HttpGateway {
    /// Returns the uninitialized warp filter to compose with other routing handling or websockets.
    pub fn as_filter() -> (Self, BoxedFilter<(impl Reply + 'static,)>) {
        let contract_web_path = std::env::temp_dir().join("locutus").join("webs");
        std::fs::create_dir_all(&contract_web_path).unwrap();

        let (request_sender, server_request) = mpsc::channel(PARALLELISM);
        let gateway = Self {
            server_request,
            response_channels: HashMap::new(),
        };

        let get_home = warp::path::end().and_then(home);
        let base_web_contract = warp::path!("contract" / "web");
        let data_contracts = warp::path!("contract" / "dependency");

        let rs = request_sender.clone();
        let web_home = base_web_contract
            .map(move || rs.clone())
            .and(warp::path::param())
            .and(warp::path::end())
            .and_then(|rs, key: String| async move {
                crate::web_handling::contract_home(key, rs).await
            });

        let web_subpages = base_web_contract
            .and(warp::path::param())
            .and(warp::filters::path::full())
            .and_then(|key: String, path| async move {
                crate::web_handling::variable_content(key, path).await
            });

        let rs = request_sender.clone();
        let get_state = data_contracts
            .map(move || rs.clone())
            .and(warp::path::param())
            .and(warp::path("get"))
            .and_then(|rs, key: String| async move { get_state(key, rs).await });

        let rs = request_sender.clone();
        let state_changes = data_contracts
            .map(move || rs.clone())
            .and(warp::path::param())
            .and(warp::path("changes"))
            .and(warp::ws())
            .map(|rs, key: String, ws: warp::ws::Ws| {
                ws.on_upgrade(move |websocket: WebSocket| {
                    state_changes_notification(key, rs, websocket)
                })
            });

        let state_update = data_contracts
            .map(move || request_sender.clone())
            .and(warp::path::param())
            .and(warp::path("update"))
            .and(warp::path::end())
            .and(warp::post())
            .and(warp::body::bytes())
            .and_then(move |rs, key: String, update_val: Bytes| async move {
                update_state(key, update_val.to_vec().into(), rs).await
            });

        let filters = get_home
            .or(web_home)
            .or(web_subpages)
            .or(get_state)
            .or(state_changes)
            .or(state_update)
            .recover(errors::handle_error)
            .with(warp::trace::request());

        (gateway, filters.boxed())
    }

    pub fn next_id() -> ClientId {
        ClientId::new(ID.fetch_add(1, Ordering::SeqCst))
    }
}

async fn home() -> Result<impl Reply, Rejection> {
    Ok(reply::reply())
}

#[derive(Debug)]
enum ExtractError {
    Io(std::io::Error),
    StripPrefixError(std::path::StripPrefixError),
}

impl From<std::io::Error> for ExtractError {
    fn from(error: std::io::Error) -> Self {
        ExtractError::Io(error)
    }
}

impl From<std::path::StripPrefixError> for ExtractError {
    fn from(error: std::path::StripPrefixError) -> Self {
        ExtractError::StripPrefixError(error)
    }
}

/// Each request is unique so we don't keep track of a client session of any sort.
static ID: AtomicUsize = AtomicUsize::new(0);

#[allow(clippy::needless_lifetimes)]
impl ClientEventsProxy for HttpGateway {
    fn recv<'a>(
        &'a mut self,
    ) -> Pin<
        Box<dyn Future<Output = Result<(ClientId, ClientRequest), ClientError>> + Send + Sync + '_>,
    > {
        Box::pin(async move {
            if let Some((req, ch_or_id)) = self.server_request.recv().await {
                match ch_or_id {
                    Either::Left(new_client_ch) => {
                        // is a new client, assign an id and open a channel to communicate responses from the node
                        tracing::debug!("received request: {req}");
                        let cli_id = ClientId::new(ID.fetch_add(1, Ordering::SeqCst));
                        self.response_channels.insert(cli_id, new_client_ch);
                        Ok((cli_id, req))
                    }
                    Either::Right(existing_client) => {
                        // just forward the request to the node
                        Ok((existing_client, req))
                    }
                }
            } else {
                todo!()
            }
        })
    }

    fn send<'a>(
        &'a mut self,
        client: ClientId,
        response: Result<HostResponse, ClientError>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ClientError>> + Send + Sync + '_>> {
        Box::pin(async move {
            if let Some(ch) = self.response_channels.remove(&client) {
                let should_rm = response
                    .as_ref()
                    .map_err(|err| matches!(err.kind(), ErrorKind::Disconnect))
                    .err()
                    .unwrap_or(false);
                if ch.send((client, response)).is_ok() && !should_rm {
                    // still alive connection, keep it
                    self.response_channels.insert(client, ch);
                }
            } else {
                log::warn!("client: {client} not found");
            }
            Ok(())
        })
    }

    fn cloned(&self) -> BoxedClient {
        unimplemented!()
    }
}
