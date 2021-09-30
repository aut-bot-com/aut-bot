pub mod path;
pub mod processors;
pub mod source;

use crate::audit_log::SearchQuery;
use crate::config::Configuration;
use crate::event::{
    Agent, Channel, Content, Entity, IdParams, NormalizedEvent, Source as EventSource,
};
use crate::gateway::path::Path;
use crate::gateway::source::{AuditLogSource, Source};
use crate::rpc::gateway_queue_lib::GatewayEvent;
use crate::rpc::logs::event::EventType;
use crate::{audit_log, util};
use anyhow::Context as _;
use futures::try_join;
use jmespath::Variable;
use slog::Logger;
use static_assertions::assert_impl_all;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use twilight_http::Client;
use twilight_model::gateway::event::EventType as GatewayEventType;
use twilight_model::guild::audit_log::AuditLogEntry;
use twilight_model::guild::Permissions;

/// Wrapper around `GatewayEvent` that includes the deserialized JSON
pub struct EventWithSource {
    inner: GatewayEvent,
    source: serde_json::Value,
}

impl TryFrom<GatewayEvent> for EventWithSource {
    type Error = rmp_serde::decode::Error;

    fn try_from(value: GatewayEvent) -> Result<Self, Self::Error> {
        let source = rmp_serde::from_slice::<serde_json::Value>(&value.inner)?;
        Ok(Self {
            inner: value,
            source,
        })
    }
}

/// Enumerates the various errors that can occur during event processing
/// that cause it to halt
#[derive(Error, Debug)]
pub enum ProcessingError {
    #[error("no sub-processor found for event type {0}")]
    SubProcessorNotFound(String),
    #[error("fatal sourcing error encountered: {0}")]
    FatalSourceError(anyhow::Error),
    #[error("dropping original gateway event")]
    Drop,
    #[error("no audit log entry for event type {0} was sourced, but it is used to source a required field")]
    NoAuditLogEntry(String),
}

impl ProcessingError {
    /// Whether the error occurs in a non-nominal case that should be logged
    pub const fn is_unexpected(&self) -> bool {
        !matches!(self, Self::Drop)
    }
}

/// Represents a collection of processors that each have
/// a corresponding gateway event type
/// and are capable of normalizing raw JSON of that type
/// into `NormalizedEvent`s
pub struct ProcessorFleet {
    processors: HashMap<String, Processor>,
    client: Client,
    config: Arc<Configuration>,
    emojis: Arc<crate::emoji::Db>,
    logger: Logger,
}

// ProcessorFleet needs to be safe to share
assert_impl_all!(ProcessorFleet: Sync);

impl ProcessorFleet {
    /// Creates a processor with an empty set of sub-processors
    #[must_use]
    pub fn new(
        client: Client,
        config: Arc<Configuration>,
        emojis: Arc<crate::emoji::Db>,
        logger: Logger,
    ) -> Self {
        Self {
            processors: HashMap::new(),
            client,
            config,
            emojis,
            logger,
        }
    }

    /// Adds a new sub-processor to this aggregate processor,
    /// working by serializing the event type into a string
    /// and adding it to the internal map
    fn register(&mut self, event_type: GatewayEventType, processor: Processor) {
        let event_key = util::value_to_string(&event_type).unwrap();
        self.processors.insert(event_key, processor);
    }

    /// Applies the main data-oriented workflow to the given JSON
    pub async fn normalize(
        &self,
        event: EventWithSource,
    ) -> Result<NormalizedEvent, ProcessingError> {
        if let Some(processor) = self.processors.get(&event.inner.event_type) {
            let logger = self.logger.new(slog::o!(
                "event_id" => event.inner.id.clone(),
                "event_ingress_timestamp" => event.inner.ingress_timestamp,
                "event_type" => event.inner.event_type.clone(),
                "event_guild_id" => event.inner.guild_id
            ));
            processor
                .apply(event, &self.client, &self.config, &self.emojis, &logger)
                .await
        } else {
            Err(ProcessingError::SubProcessorNotFound(
                event.inner.event_type,
            ))
        }
    }
}

pub enum Processor {
    Split(SplitProcessor),
    Direct(Source<NormalizedEvent>),
}

impl Processor {
    /// Runs a single processor, attempting to create a Normalized Event as a result
    pub async fn apply<'a>(
        &self,
        event: EventWithSource,
        client: &'a Client,
        config: &'a Configuration,
        emojis: &'a crate::emoji::Db,
        logger: &'a Logger,
    ) -> Result<NormalizedEvent, ProcessingError> {
        match self {
            Self::Split(split) => split.apply(event, client, config, emojis, logger).await,
            Self::Direct(direct_source) => {
                let EventWithSource {
                    inner: event,
                    source,
                } = event;

                let ctx = Context {
                    event: &event,
                    source: &source,
                    audit_log_entry: None,
                    client,
                    config,
                    emojis,
                    logger,
                };

                direct_source.consume(ctx).await
            }
        }
    }
}

pub struct SplitProcessor {
    event_type: Source<EventType>,
    id_params: Source<IdParams>,
    audit_log: Option<AuditLogSource>,
    timestamp: Source<u64>,
    reason: Source<Option<String>>,
    channel: Source<Option<Channel>>,
    agent: Source<Option<Agent>>,
    subject: Source<Option<Entity>>,
    auxiliary: Source<Option<Entity>>,
    content: Source<Content>,
}

// ProcessorFleet needs to be safe to share
assert_impl_all!(Processor: Sync);

impl SplitProcessor {
    /// Runs a single processor, attempting to create a Normalized Event as a result
    pub async fn apply<'a>(
        &self,
        event: EventWithSource,
        client: &'a Client,
        config: &'a Configuration,
        emojis: &'a crate::emoji::Db,
        logger: &'a Logger,
    ) -> Result<NormalizedEvent, ProcessingError> {
        let EventWithSource {
            inner: event,
            source,
        } = event;

        // Create a RwLock that source objects can wait on if needed
        let audit_log_lock: RwLock<Option<CombinedAuditLogEntry>> = RwLock::new(None);
        let ctx = Context {
            event: &event,
            source: &source,
            audit_log_entry: Some(LockReader::new(&audit_log_lock)),
            client,
            config,
            emojis,
            logger,
        };

        let write_lock = if self.audit_log.is_some() {
            // Acquire a write lock and then move it into a future,
            // so that any readers will always block when entering their futures
            Some(audit_log_lock.write().await)
        } else {
            None
        };

        // Run each source in parallel
        let (
            _,
            event_type,
            id_params,
            timestamp,
            reason,
            channel,
            agent,
            subject,
            auxiliary,
            content,
        ) = try_join!(
            self.load_audit_log(write_lock, ctx.clone()),
            self.event_type.consume(ctx.clone()),
            self.id_params.consume(ctx.clone()),
            self.timestamp.consume(ctx.clone()),
            self.reason.consume(ctx.clone()),
            self.channel.consume(ctx.clone()),
            self.agent.consume(ctx.clone()),
            self.subject.consume(ctx.clone()),
            self.auxiliary.consume(ctx.clone()),
            self.content.consume(ctx.clone()),
        )?;

        drop(ctx);
        let guild_id = event.guild_id;
        let audit_log_entry = audit_log_lock.into_inner();
        let audit_log_id = audit_log_entry.as_ref().map(|combined| combined.entry.id.0);
        let audit_log_json = audit_log_entry.map(|combined| combined.json);
        let source = EventSource {
            gateway: Some(source),
            audit_log: audit_log_json,
            ..EventSource::default()
        };
        let origin = source.origin();

        Ok(NormalizedEvent {
            id_params,
            timestamp,
            source,
            origin,
            event_type,
            guild_id,
            reason,
            audit_log_id,
            channel,
            agent,
            subject,
            auxiliary,
            content,
        })
    }

    /// Asynchronously loads an audit log entry,
    /// taking in a write lock at the beginning to ensure that any readers
    /// are blocked until loading is complete (if performed)
    async fn load_audit_log(
        &self,
        write_lock: Option<RwLockWriteGuard<'_, Option<CombinedAuditLogEntry>>>,
        context: Context<'_>,
    ) -> Result<(), ProcessingError> {
        if let Some(audit_log_source) = &self.audit_log {
            // Invariant: if audit_log_source is non-none, then write_lock is too
            let mut write_lock = write_lock.unwrap();
            match audit_log_source.consume(context).await {
                // Lock is released upon returning
                Err(err) => Err(err),
                Ok(None) => Ok(()),
                Ok(Some(audit_log_entry)) => {
                    let audit_log_json = serde_json::to_value(&audit_log_entry)
                        .with_context(|| {
                            format!(
                                "could not serialize audit log entry to JSON: {:?}",
                                audit_log_entry
                            )
                        })
                        .map_err(ProcessingError::FatalSourceError)?;
                    *write_lock = Some(CombinedAuditLogEntry {
                        entry: audit_log_entry,
                        json: audit_log_json,
                    });
                    Ok(())
                }
            }
        } else {
            Ok(())
        }
    }
}

/// Represents a sourced audit log entry
/// that has been pre-serialized to JSON
/// to use with path-based field sources
#[derive(Clone, Debug)]
pub struct CombinedAuditLogEntry {
    entry: AuditLogEntry,
    json: serde_json::Value,
}

/// Struct of borrows/references to various values
/// that might be useful when normalizing an incoming event,
/// including the source data.
/// Can be cheaply cloned.
#[derive(Clone, Debug)]
pub struct Context<'a> {
    event: &'a GatewayEvent,
    source: &'a serde_json::Value,
    audit_log_entry: Option<LockReader<'a, Option<CombinedAuditLogEntry>>>,
    client: &'a Client,
    config: &'a Configuration,
    emojis: &'a crate::emoji::Db,
    logger: &'a Logger,
}

#[allow(dead_code)]
impl Context<'_> {
    /// Attempts to extract a gateway value
    pub fn gateway<T, F>(&self, path: &Path, extractor: F) -> Result<T, anyhow::Error>
    where
        F: (Fn(&Variable, Context<'_>) -> Result<T, anyhow::Error>) + Send + Sync + 'static,
    {
        path.extract(self.source, &extractor, self.clone())
    }

    /// Attempts to extract an audit log value
    pub async fn audit_log<T, F>(&self, path: &Path, extractor: F) -> Result<T, anyhow::Error>
    where
        F: (Fn(&Variable, Context<'_>) -> Result<T, anyhow::Error>) + Send + Sync + 'static,
    {
        let audit_log_read = self
            .audit_log_entry
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no audit log reader lock"))?
            .read()
            .await;
        let audit_log_entry = audit_log_read.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "no audit log entry was parsed for event type {}",
                self.event.event_type
            )
        })?;
        path.extract(&audit_log_entry.json, &extractor, self.clone())
    }

    /// Determines whether the Architus user has permissions in the guild for this event's context
    pub async fn has_perms(&self, _permissions: Permissions) -> Result<bool, anyhow::Error> {
        // TODO implement
        Ok(true)
    }

    /// Runs an audit log search on the guild for this event's context
    pub async fn get_audit_log_entry<P>(
        &self,
        search: SearchQuery<P>,
    ) -> Result<AuditLogEntry, anyhow::Error>
    where
        P: Fn(&AuditLogEntry) -> bool,
    {
        audit_log::get_entry(self.client, search)
            .await
            .with_context(|| {
                format!(
                    "audit log search failed for event type {}",
                    self.event.event_type
                )
            })
    }
}

#[derive(Clone, Debug)]
pub struct LockReader<'a, T> {
    inner: &'a RwLock<T>,
}

impl<'a, T> LockReader<'a, T> {
    /// Asynchronously obtains a read-only handle to the inner lock
    #[allow(clippy::future_not_send)]
    pub async fn read(&self) -> RwLockReadGuard<'_, T> {
        self.inner.read().await
    }

    const fn new(lock: &'a RwLock<T>) -> Self {
        Self { inner: lock }
    }
}
