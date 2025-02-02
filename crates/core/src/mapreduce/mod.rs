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

//! MapReduce is a distributed computing framework for processing large data sets across
//! clusters of computers using a master node to coordinate the work.

use serde::{de::DeserializeOwned, Deserialize, Serialize};

mod manager;
mod worker;

pub use manager::Manager;
use thiserror::Error;
pub use worker::StatelessWorker;
pub use worker::Worker;

use crate::distributed::sonic;

pub(crate) type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("network error")]
    Sonic(#[from] sonic::Error),

    #[error("could not get a working worker")]
    NoAvailableWorker,

    #[error("did not get a reponse")]
    NoResponse,
}

pub trait Map<W, T>
where
    Self: Serialize + DeserializeOwned + Send,
    T: Serialize + DeserializeOwned + Send,
    W: Worker,
{
    fn map(&self, worker: &W) -> T;
}

pub trait Reduce<T> {
    #[must_use]
    fn reduce(self, element: T) -> Self;
}

#[derive(Serialize, Deserialize, Debug)]
enum Task<T> {
    Job(T),
    AllFinished,
}

type MapReduceServer<I, O> = sonic::Server<Task<I>, Option<O>>;
type MapReduceConnection<I, O> = sonic::Connection<Task<I>, Option<O>>;
