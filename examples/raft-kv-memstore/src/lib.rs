#![allow(clippy::uninlined_format_args)]
#![deny(unused_qualifications)]

use std::sync::Arc;

use actix_web::middleware;
use actix_web::middleware::Logger;
use actix_web::web::Data;
use actix_web::App;
use actix_web::HttpServer;
use openraft::BasicNode;
use openraft::Config;
use openraft::Raft;

use crate::app::ExampleApp;
use crate::network::api;
use crate::network::management;
use crate::network::raft;
use crate::network::raft_network_impl::ExampleNetwork;
use crate::store::ExampleRequest;
use crate::store::ExampleResponse;
use crate::store::ExampleStore;

pub mod app;
pub mod client;
pub mod network;
pub mod store;

pub type ExampleNodeId = u64;

openraft::declare_raft_types!(
    /// Declare the type configuration for example K/V store.
    pub ExampleTypeConfig: D = ExampleRequest, R = ExampleResponse, NodeId = ExampleNodeId, Node = BasicNode, Entry = openraft::Entry<ExampleTypeConfig>
);

pub type ExampleRaft = Raft<ExampleTypeConfig, ExampleNetwork, Arc<ExampleStore>>;

pub mod typ {
    use openraft::BasicNode;

    use crate::ExampleNodeId;
    use crate::ExampleTypeConfig;

    pub type RaftError<E = openraft::error::Infallible> = openraft::error::RaftError<ExampleNodeId, E>;
    pub type RPCError<E = openraft::error::Infallible> =
        openraft::error::RPCError<ExampleNodeId, BasicNode, RaftError<E>>;

    pub type ClientWriteError = openraft::error::ClientWriteError<ExampleNodeId, BasicNode>;
    pub type CheckIsLeaderError = openraft::error::CheckIsLeaderError<ExampleNodeId, BasicNode>;
    pub type ForwardToLeader = openraft::error::ForwardToLeader<ExampleNodeId, BasicNode>;
    pub type InitializeError = openraft::error::InitializeError<ExampleNodeId, BasicNode>;

    pub type ClientWriteResponse = openraft::raft::ClientWriteResponse<ExampleTypeConfig>;
}

pub async fn start_example_raft_node(node_id: ExampleNodeId, http_addr: String) -> std::io::Result<()> {
    // Create a configuration for the raft instance.
    let config = Config {
        heartbeat_interval: 500,
        election_timeout_min: 1500,
        election_timeout_max: 3000,
        ..Default::default()
    };

    let config = Arc::new(config.validate().unwrap());

    // Create a instance of where the Raft data will be stored.
    let store = Arc::new(ExampleStore::default());

    // Create the network layer that will connect and communicate the raft instances and
    // will be used in conjunction with the store created above.
    let network = ExampleNetwork {};

    // Create a local raft instance.
    let raft = Raft::new(node_id, config.clone(), network, store.clone()).await.unwrap();

    // Create an application that will store all the instances created above, this will
    // be later used on the actix-web services.
    let app = Data::new(ExampleApp {
        id: node_id,
        addr: http_addr.clone(),
        raft,
        store,
        config,
    });

    // Start the actix-web server.
    let server = HttpServer::new(move || {
        App::new()
            .wrap(Logger::default())
            .wrap(Logger::new("%a %{User-Agent}i"))
            .wrap(middleware::Compress::default())
            .app_data(app.clone())
            // raft internal RPC
            .service(raft::append)
            .service(raft::snapshot)
            .service(raft::vote)
            // admin API
            .service(management::init)
            .service(management::add_learner)
            .service(management::change_membership)
            .service(management::metrics)
            // application API
            .service(api::write)
            .service(api::read)
            .service(api::consistent_read)
    });

    let x = server.bind(http_addr)?;

    x.run().await
}
