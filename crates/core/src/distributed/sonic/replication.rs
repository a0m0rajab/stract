// Stract is an open source web search engine.
// Copyright (C) 2023 Stract ApS
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

use futures::future::join_all;
use rand::seq::IteratorRandom;

use super::Result;
use crate::distributed::{retry_strategy::ExponentialBackoff, sonic};
use std::{net::SocketAddr, time::Duration};

#[derive(Debug, Clone)]
pub struct RemoteClient<S: sonic::service::Service> {
    addr: SocketAddr,
    _phantom: std::marker::PhantomData<S>,
}

impl<S> RemoteClient<S>
where
    S: sonic::service::Service,
{
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<S> RemoteClient<S>
where
    S: sonic::service::Service,
{
    async fn conn(&self) -> Result<sonic::service::ResilientConnection<S>> {
        let retry = ExponentialBackoff::from_millis(30)
            .with_limit(Duration::from_millis(200))
            .take(5);

        sonic::service::ResilientConnection::create_with_timeout(
            self.addr,
            Duration::from_secs(30),
            retry,
        )
        .await
    }

    async fn send<R: sonic::service::Wrapper<S>>(&self, req: &R) -> Result<R::Response> {
        let conn = self.conn().await?;
        conn.send_with_timeout(req, Duration::from_secs(60)).await
    }
}

pub trait ReplicaSelector<S: sonic::service::Service> {
    fn select<'a>(&self, replicas: &'a [RemoteClient<S>]) -> Vec<&'a RemoteClient<S>>;
}

pub struct RandomReplicaSelector;

impl<S> ReplicaSelector<S> for RandomReplicaSelector
where
    S: sonic::service::Service,
{
    fn select<'a>(&self, replicas: &'a [RemoteClient<S>]) -> Vec<&'a RemoteClient<S>> {
        let mut rng = rand::thread_rng();
        replicas.iter().choose_multiple(&mut rng, 1)
    }
}

pub struct AllReplicaSelector;

impl<S> ReplicaSelector<S> for AllReplicaSelector
where
    S: sonic::service::Service,
{
    fn select<'a>(&self, replicas: &'a [RemoteClient<S>]) -> Vec<&'a RemoteClient<S>> {
        replicas.iter().collect()
    }
}

pub struct ReplicatedClient<S: sonic::service::Service> {
    clients: Vec<RemoteClient<S>>,
}

impl<S> ReplicatedClient<S>
where
    S: sonic::service::Service,
{
    pub fn new(clients: Vec<RemoteClient<S>>) -> Self {
        Self { clients }
    }

    pub async fn send<Req, Rep>(&self, req: &Req, selector: &Rep) -> Result<Vec<Req::Response>>
    where
        Req: sonic::service::Wrapper<S>,
        Rep: ReplicaSelector<S>,
    {
        let mut futures = Vec::new();
        for client in selector.select(&self.clients) {
            futures.push(client.send(req));
        }

        let mut results = Vec::new();
        for r in join_all(futures).await {
            match r {
                Ok(r) => results.push(r),
                Err(e) => {
                    tracing::error!("Failed to send request: {:?}", e);
                }
            }
        }

        Ok(results)
    }
}

pub trait ShardIdentifier: PartialEq + Eq + Clone {}

pub trait ShardSelector<S: sonic::service::Service, Id: ShardIdentifier> {
    fn select<'a>(&self, shards: &'a [Shard<S, Id>]) -> Vec<&'a Shard<S, Id>>;
}

pub struct AllShardsSelector;

impl<S, Id> ShardSelector<S, Id> for AllShardsSelector
where
    S: sonic::service::Service,
    Id: ShardIdentifier,
{
    fn select<'a>(&self, shards: &'a [Shard<S, Id>]) -> Vec<&'a Shard<S, Id>> {
        shards.iter().collect()
    }
}

pub struct RandomShardSelector;

impl<S, Id> ShardSelector<S, Id> for RandomShardSelector
where
    S: sonic::service::Service,
    Id: ShardIdentifier,
{
    fn select<'a>(&self, shards: &'a [Shard<S, Id>]) -> Vec<&'a Shard<S, Id>> {
        let mut rng = rand::thread_rng();
        shards.iter().choose_multiple(&mut rng, 1)
    }
}

pub struct SpecificShardSelector<Id: ShardIdentifier>(pub Id);

impl<S, Id> ShardSelector<S, Id> for SpecificShardSelector<Id>
where
    S: sonic::service::Service,
    Id: ShardIdentifier,
{
    fn select<'a>(&self, shards: &'a [Shard<S, Id>]) -> Vec<&'a Shard<S, Id>> {
        shards.iter().find(|s| s.id == self.0).into_iter().collect()
    }
}

pub struct Shard<S: sonic::service::Service, Id: ShardIdentifier> {
    replicas: ReplicatedClient<S>,
    id: Id,
}

impl<S, Id> Shard<S, Id>
where
    S: sonic::service::Service,
    Id: ShardIdentifier,
{
    pub fn new(id: Id, replicas: ReplicatedClient<S>) -> Self {
        Self { replicas, id }
    }
}

pub struct ShardedClient<S: sonic::service::Service, Id: ShardIdentifier> {
    shards: Vec<Shard<S, Id>>,
}

impl<S, Id> ShardedClient<S, Id>
where
    S: sonic::service::Service,
    Id: ShardIdentifier,
{
    pub fn new(shards: Vec<Shard<S, Id>>) -> Self {
        Self { shards }
    }

    async fn send_single<Req, RSel>(
        &self,
        req: &Req,
        shard: &Shard<S, Id>,
        replica_selector: &RSel,
    ) -> Result<(Id, Vec<Req::Response>)>
    where
        Req: sonic::service::Wrapper<S>,
        RSel: ReplicaSelector<S>,
    {
        Ok((
            shard.id.clone(),
            shard.replicas.send(req, replica_selector).await?,
        ))
    }

    pub async fn send<Req, SSel, RSel>(
        &self,
        req: &Req,
        shard_selector: &SSel,
        replica_selector: &RSel,
    ) -> Result<Vec<(Id, Vec<Req::Response>)>>
    where
        Req: sonic::service::Wrapper<S>,
        SSel: ShardSelector<S, Id>,
        RSel: ReplicaSelector<S>,
    {
        let mut futures = Vec::new();
        for shard in shard_selector.select(&self.shards) {
            futures.push(self.send_single(req, shard, replica_selector));
        }

        let mut results = Vec::new();
        for r in join_all(futures).await {
            match r {
                Ok(r) => results.push(r),
                Err(e) => {
                    tracing::error!("Failed to send request: {:?}", e);
                }
            }
        }

        Ok(results)
    }
}
