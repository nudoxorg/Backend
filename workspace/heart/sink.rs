//! The remote-upload contract every derived store speaks.
//!
//! Exposes an extension trait, `SinkExt`, which adds retry-aware delivery
//! directly to any qualifying `tower::Service`.

use std::future::{Ready, ready};

use serde::{Deserialize, Serialize};
use tower::{
    retry::{Policy, Retry},
    Service, ServiceExt,
};

use crate::error::Retryable;

/// Which derived store a record fans out to.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::EnumIter,
)]
pub enum DerivedStore {
    Vector,
    Graph,
    Text,
}

#[derive(Debug, Clone)]
pub struct RetryTransient {
    remaining: usize,
}

impl RetryTransient {
    pub const fn new(budget: usize) -> Self {
        Self { remaining: budget }
    }
}

impl<Req, Res, E> Policy<Req, Res, E> for RetryTransient
where
    Req: Clone,
    E: Retryable,
{
    type Future = Ready<RetryTransient>;

    fn retry(&self, _req: &Req, result: Result<&Res, &E>) -> Option<Ready<RetryTransient>> {
        match result {
            Err(e) if self.remaining > 0 && e.is_retryable() => {
                Some(ready(RetryTransient { remaining: self.remaining - 1 }))
            }
            _ => None,
        }
    }

    fn clone_request(&self, req: &Req) -> Option<Req> {
        Some(req.clone())
    }
}

/// Equips any compatible `tower::Service` with transient-retrying delivery.
pub trait SinkExt<Req>: Service<Req> + Clone + Sized
where
    Req: Clone,
    Self::Error: Retryable,
{
    const DEFAULT_BUDGET: usize = 4;

    async fn deliver(&self, payload: Req) -> Result<Self::Response, Self::Error> {
        self.deliver_with_budget(payload, Self::DEFAULT_BUDGET).await
    }

    async fn deliver_with_budget(
        &self,
        payload: Req,
        budget: usize,
    ) -> Result<Self::Response, Self::Error> {
        Retry::new(RetryTransient::new(budget), self.clone())
            .oneshot(payload)
            .await
    }
}

impl<S, Req> SinkExt<Req> for S
where
    S: Service<Req> + Clone,
    Req: Clone,
    S::Error: Retryable,
{
}
