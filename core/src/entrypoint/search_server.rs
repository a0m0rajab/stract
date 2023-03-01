// Cuely is an open source web search engine.
// Copyright (C) 2022 Cuely ApS
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use std::net::SocketAddr;

use crate::{
    cluster::{
        member::{Member, Service},
        Cluster,
    },
    entity_index::EntityIndex,
    index::Index,
    inverted_index,
    ranking::centrality_store::SearchCentralityStore,
    searcher::{self, LocalSearcher},
    sonic, Result, SearchServerConfig,
};

pub async fn run(config: SearchServerConfig) -> Result<()> {
    let addr: SocketAddr = config.host;
    let server = sonic::Server::bind(addr).await.unwrap();
    tracing::info!("listening on {}", addr);

    let entity_index = config
        .entity_index_path
        .map(|path| EntityIndex::open(path).unwrap());
    let centrality_store = config
        .centrality_store_path
        .map(SearchCentralityStore::open);
    let search_index = Index::open(config.index_path)?;

    let mut local_searcher = LocalSearcher::new(search_index);

    if let Some(entity_index) = entity_index {
        local_searcher.set_entity_index(entity_index);
    }

    if let Some(centrality_store) = centrality_store {
        local_searcher.set_centrality_store(centrality_store);
    }

    // dropping the handle leaves the cluster
    let _cluster_handle = Cluster::join(
        Member {
            id: config.cluster_id,
            service: Service::Searcher {
                host: config.host,
                shard: config.shard_id,
            },
        },
        config.gossip_addr,
        config.gossip_seed_nodes.unwrap_or_default(),
    )
    .await?;

    loop {
        if let Ok(req) = server.accept::<searcher::distributed::Request>().await {
            match &req.body {
                searcher::Request::RetrieveWebsites { websites, query } => {
                    match local_searcher.retrieve_websites(websites, query) {
                        Ok(response) => {
                            req.respond(sonic::Response::Content(response)).await.ok();
                        }
                        Err(_) => {
                            req.respond::<Vec<inverted_index::RetrievedWebpage>>(
                                sonic::Response::Empty,
                            )
                            .await
                            .ok();
                        }
                    }
                }
                searcher::Request::Search(query) => {
                    match local_searcher.search_initial(query, false) {
                        Ok(result) => {
                            req.respond(sonic::Response::Content(result)).await.ok();
                        }
                        Err(_) => {
                            req.respond::<inverted_index::SearchResult>(sonic::Response::Empty)
                                .await
                                .ok();
                        }
                    }
                }
                searcher::Request::GetWebpage { url } => {
                    let result = local_searcher.get_webpage(url);
                    req.respond(sonic::Response::Content(result)).await.ok();
                }
            }
        }
    }
}
