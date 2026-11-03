use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use facet_core::Shape;
use futures_util::FutureExt as _;
use tracing::{trace, warn};
use vox_rt::sync::{mpsc, oneshot, watch};
use vox_types::time::Instant;
use vox_types::{
    BoxFut, ChannelMessage, ConduitRx, ConduitTx, ConnectionRole, ConnectionSettings, Decline,
    EstablishmentContext, EstablishmentDetails, EstablishmentEvent, EstablishmentOutcome,
    EstablishmentPhase, Handler, HandshakeResult, IdAllocator, IdentityResolutionContext,
    LaneAccept, LaneClose, LaneGrant, LaneId, LaneOpen, LaneReject, MaybeSend, MaybeSync, Message,
    MessageFamily, MessagePayload, Metadata, Parity, PeerEvidence, PeerIdentity, RequestBody,
    RequestId, RequestMessage, RequestResponse, SchemaMessage, SelfRef, TrySendError,
    VoxDebugSnapshot, VoxObserverHandle,
};
use vox_types::{
    ConnectionCloseReason, DecodeErrorKind, DriverTaskStatus, LaneDebugSnapshot, LaneDebugState,
};

mod builders;
// Failure outcomes are implemented but not yet wired to live callers.
pub(crate) mod channel_gate_lifecycle;
// Retirement currently gates admission only; write leases and response
// identities are not yet integrated into the connection shutdown protocol.
#[allow(dead_code)]
mod lane_retirement;
#[allow(dead_code)]
pub(crate) mod response_reservation;
mod wake_registry;
pub use builders::*;
use channel_gate_lifecycle::{ChannelDeclarationGate, GateState};
pub(crate) use lane_retirement::LaneLifetime;

/// Connection-level protocol keepalive configuration.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionKeepaliveConfig {
    pub ping_interval: Duration,
    pub pong_timeout: Duration,
}

/// Opt-in per-connection resource ceilings for untrusted peers.
///
/// Lane-open, peer-call, channel-ID, and schema budgets are lifetime quotas: exceeding a
/// quota closes the connection. Reconnect to obtain a fresh
/// budget. These limits do not constrain the transport, decoder, application
/// allocations, or locally initiated work; configure those independently.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionResourceLimits {
    /// Simultaneously active or pending service lanes (control lane excluded).
    pub max_service_lanes: usize,
    /// Total peer lane-open attempts over this connection's lifetime.
    pub max_peer_lane_opens: usize,
    /// Maximum locally advertised concurrent requests on an incoming lane.
    pub max_inbound_requests_per_lane: u32,
    /// Maximum locally advertised initial per-channel credit.
    pub max_initial_channel_credit: u32,
    /// Largest peer initial credit and, for Driver lanes, available credit
    /// after a grant. Raw and proxy lane consumers must enforce grants themselves.
    pub max_peer_channel_credit: u32,
    /// Maximum peer-advertised concurrent request limit accepted.
    pub max_peer_concurrent_requests: u32,
    /// Total peer call messages accepted over one lane's lifetime.
    pub max_peer_calls_per_lane: usize,
    /// Distinct peer-supplied channel IDs over one lane's lifetime.
    pub max_peer_channel_ids_per_lane: usize,
    /// Schema messages or inline schemas accepted over one lane's lifetime.
    pub max_schemas_per_lane: usize,
    /// Sum of bytes in those schemas, counting replacements again.
    pub max_schema_bytes_per_lane: usize,
    /// Maximum time to process one incoming message, including queue and
    /// control-write backpressure. Exceeding it closes the connection.
    pub max_message_processing_time: Duration,
}

pub const VOX_LANE_REJECT_REASON_METADATA_KEY: &str = "vox-lane-reject-reason";
pub const VOX_LANE_REJECT_MESSAGE_METADATA_KEY: &str = "vox-lane-reject-message";

// r[impl lane.open.result]
// r[impl lane.authorization]
// r[impl lane.authorization.filtered]
// r[impl rejection.reason.taxonomy]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneRejectReason {
    UnknownService,
    Forbidden,
    NotReady,
    Draining,
    SchemaIncompatible,
    PolicyRejected,
}

impl LaneRejectReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownService => "unknown-service",
            Self::Forbidden => "forbidden",
            Self::NotReady => "not-ready",
            Self::Draining => "draining",
            Self::SchemaIncompatible => "schema-incompatible",
            Self::PolicyRejected => "policy-rejected",
        }
    }

    #[must_use]
    pub fn from_metadata_value(value: &str) -> Option<Self> {
        match value {
            "unknown-service" => Some(Self::UnknownService),
            "forbidden" => Some(Self::Forbidden),
            "not-ready" => Some(Self::NotReady),
            "draining" => Some(Self::Draining),
            "schema-incompatible" => Some(Self::SchemaIncompatible),
            "policy-rejected" => Some(Self::PolicyRejected),
            _ => None,
        }
    }
}

impl std::fmt::Display for LaneRejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// r[impl lane.open.result]
// r[impl lane.authorization]
// r[impl lane.authorization.filtered]
#[derive(Debug, Clone)]
pub struct LaneRejection {
    reason: LaneRejectReason,
    metadata: Metadata,
}

impl LaneRejection {
    #[must_use]
    pub fn new(reason: LaneRejectReason) -> Self {
        Self::with_metadata(reason, Metadata::default())
    }

    #[must_use]
    pub fn with_message(reason: LaneRejectReason, message: impl Into<String>) -> Self {
        let metadata = vox_types::metadata()
            .str(VOX_LANE_REJECT_MESSAGE_METADATA_KEY, message.into())
            .build();
        Self::with_metadata(reason, metadata)
    }

    #[must_use]
    pub fn with_metadata(reason: LaneRejectReason, mut metadata: Metadata) -> Self {
        vox_types::meta_set(
            &mut metadata,
            VOX_LANE_REJECT_REASON_METADATA_KEY,
            reason.as_str(),
        );
        Self { reason, metadata }
    }

    #[must_use]
    pub fn from_metadata(metadata: Metadata) -> Self {
        let reason = vox_types::metadata_get_str(&metadata, VOX_LANE_REJECT_REASON_METADATA_KEY)
            .and_then(LaneRejectReason::from_metadata_value)
            .unwrap_or(LaneRejectReason::PolicyRejected);
        Self::with_metadata(reason, metadata)
    }

    #[must_use]
    pub fn reason(&self) -> LaneRejectReason {
        self.reason
    }

    #[must_use]
    pub fn message(&self) -> Option<&str> {
        vox_types::metadata_get_str(&self.metadata, VOX_LANE_REJECT_MESSAGE_METADATA_KEY)
            .or_else(|| vox_types::metadata_get_str(&self.metadata, "error"))
    }

    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    #[must_use]
    pub fn into_metadata(self) -> Metadata {
        self.metadata
    }
}

impl Default for LaneRejection {
    fn default() -> Self {
        Self::new(LaneRejectReason::PolicyRejected)
    }
}

fn lane_rejection_details(rejection: &LaneRejection) -> EstablishmentDetails {
    EstablishmentDetails::rejection_reason(rejection.reason().as_str())
}

// r[impl rpc.observability.establishment]
pub(crate) fn observe_establishment_started(
    observer: Option<&VoxObserverHandle>,
    role: ConnectionRole,
    phase: EstablishmentPhase,
    lane_id: Option<LaneId>,
) -> Instant {
    let started_at = Instant::now();
    if let Some(observer) = observer {
        observer.establishment_event(EstablishmentEvent::Started {
            context: EstablishmentContext {
                role,
                phase,
                lane_id,
            },
        });
    }
    started_at
}

// r[impl rpc.observability.establishment]
pub(crate) fn observe_establishment_finished(
    observer: Option<&VoxObserverHandle>,
    role: ConnectionRole,
    phase: EstablishmentPhase,
    lane_id: Option<LaneId>,
    outcome: EstablishmentOutcome,
    started_at: Instant,
) {
    observe_establishment_finished_with_details(
        observer,
        role,
        phase,
        lane_id,
        outcome,
        started_at,
        EstablishmentDetails::EMPTY,
    );
}

pub(crate) fn observe_establishment_finished_with_details(
    observer: Option<&VoxObserverHandle>,
    role: ConnectionRole,
    phase: EstablishmentPhase,
    lane_id: Option<LaneId>,
    outcome: EstablishmentOutcome,
    started_at: Instant,
    details: EstablishmentDetails,
) {
    if let Some(observer) = observer {
        observer.establishment_event(EstablishmentEvent::Finished {
            context: EstablishmentContext {
                role,
                phase,
                lane_id,
            },
            outcome,
            elapsed: started_at.elapsed(),
            details,
        });
    }
}

// ---------------------------------------------------------------------------
// Connection acceptor trait
// ---------------------------------------------------------------------------

/// Resolves the counterpart identity during the connection handshake.
// r[impl connection.identity.resolver]
pub trait IdentityResolver: MaybeSend + MaybeSync + 'static {
    fn resolve(&self, context: IdentityResolutionContext<'_>) -> Result<PeerIdentity, Decline>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AnonymousIdentityResolver;

impl IdentityResolver for AnonymousIdentityResolver {
    fn resolve(&self, _context: IdentityResolutionContext<'_>) -> Result<PeerIdentity, Decline> {
        Ok(PeerIdentity::anonymous())
    }
}

pub struct IdentityResolverFn<F>(pub F);

impl<F> IdentityResolver for IdentityResolverFn<F>
where
    F: for<'a> Fn(IdentityResolutionContext<'a>) -> Result<PeerIdentity, Decline>
        + MaybeSend
        + MaybeSync
        + 'static,
{
    fn resolve(&self, context: IdentityResolutionContext<'_>) -> Result<PeerIdentity, Decline> {
        (self.0)(context)
    }
}

pub fn identity_resolver_fn<F>(f: F) -> IdentityResolverFn<F>
where
    F: for<'a> Fn(IdentityResolutionContext<'a>) -> Result<PeerIdentity, Decline>
        + MaybeSend
        + MaybeSync
        + 'static,
{
    IdentityResolverFn(f)
}

/// Metadata wrapper with typed getters for well-known `vox-*` keys.
///
/// Passed to [`LaneAcceptor::accept`] when a peer opens a service lane.
// r[impl lane.authorization]
pub struct LaneRequest<'a> {
    metadata: &'a vox_types::Metadata,
    service: &'a str,
    peer_identity: &'a PeerIdentity,
    peer_evidence: &'a PeerEvidence,
}

impl<'a> LaneRequest<'a> {
    /// Build a lane-open request from metadata.
    ///
    /// Returns an error if the required `vox-service` metadata key is missing.
    pub fn new(
        metadata: &'a vox_types::Metadata,
        peer_identity: &'a PeerIdentity,
        peer_evidence: &'a PeerEvidence,
    ) -> Result<Self, ConnectionError> {
        let service = vox_types::metadata_get_str(metadata, "vox-service").ok_or_else(|| {
            ConnectionError::Protocol("missing required vox-service metadata".into())
        })?;
        Ok(Self {
            metadata,
            service,
            peer_identity,
            peer_evidence,
        })
    }

    /// The requested service name (`vox-service` metadata key).
    pub fn service(&self) -> &str {
        self.service
    }

    /// The transport type (`vox-transport` metadata key).
    pub fn transport(&self) -> Option<&str> {
        vox_types::metadata_get_str(self.metadata, "vox-transport")
    }

    /// The peer address (`vox-peer-addr` metadata key).
    pub fn peer_addr(&self) -> Option<&str> {
        vox_types::metadata_get_str(self.metadata, "vox-peer-addr")
    }

    /// Look up a string value by key.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        vox_types::metadata_get_str(self.metadata, key)
    }

    /// Look up a u64 value by key.
    pub fn get_u64(&self, key: &str) -> Option<u64> {
        vox_types::metadata_get_u64(self.metadata, key)
    }

    /// Access the raw metadata map.
    pub fn metadata(&self) -> &'a vox_types::Metadata {
        self.metadata
    }

    /// The immutable identity resolved for the peer when the connection was established.
    // r[impl lane.authorization.context]
    pub fn peer_identity(&self) -> &'a PeerIdentity {
        self.peer_identity
    }

    /// Locally asserted evidence that contributed to connection identity resolution.
    // r[impl lane.authorization.context]
    pub fn peer_evidence(&self) -> &'a PeerEvidence {
        self.peer_evidence
    }
}

/// A service lane that has been opened but not yet accepted.
///
/// The acceptor receives this and decides its fate by calling one of:
/// - `handle_with(handler)` — run a Driver with this handler (common case)
/// - `proxy_to(other_handle)` — pipe messages to/from another lane
/// - `into_handle()` — take the raw LaneHandle for custom use
///
/// To reject a lane, the `LaneAcceptor` should return `Err(LaneRejection)`.
/// Dropping an unconsumed `PendingLane` is only a local safety fallback.
pub struct PendingLane {
    handle: Option<LaneHandle>,
    lane_grant: LaneGrant,
}

impl PendingLane {
    fn new(handle: LaneHandle) -> Self {
        Self {
            handle: Some(handle),
            lane_grant: LaneGrant::empty(),
        }
    }

    /// Attach the local lane grant that authorizes requests on this lane.
    #[must_use]
    // r[impl lane.authorization.context]
    pub fn with_grant(mut self, grant: LaneGrant) -> Self {
        self.lane_grant = grant;
        self
    }

    fn take_handle(&mut self) -> LaneHandle {
        let mut handle = self.handle.take().expect("PendingLane already consumed");
        handle.set_lane_grant(self.lane_grant.clone());
        handle
    }

    /// Accept this service lane and run a Driver with the given handler.
    pub fn handle_with(mut self, handler: impl Handler<crate::DriverReplySink> + 'static) {
        let handle = self.take_handle();
        let conn_id = handle.lane_id();
        trace!(%conn_id, "PendingLane::handle_with: creating driver");
        let mut driver = crate::Driver::new(handle, handler);
        #[cfg(not(target_arch = "wasm32"))]
        tokio::spawn(async move {
            trace!(%conn_id, "PendingLane driver starting");
            driver.run().await;
            trace!(%conn_id, "PendingLane driver exited");
        });
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move { driver.run().await });
    }

    /// Accept this service lane, run a Driver, and return a typed client for the peer.
    pub fn handle_with_client<C: crate::FromVoxLane>(
        mut self,
        handler: impl Handler<crate::DriverReplySink> + 'static,
    ) -> C {
        let handle = self.take_handle();
        let conn_id = handle.lane_id();
        trace!(%conn_id, "PendingLane::handle_with_client: creating driver");
        let mut driver = crate::Driver::new(handle, handler);
        let caller = crate::Caller::new(driver.caller());
        #[cfg(not(target_arch = "wasm32"))]
        tokio::spawn(async move {
            trace!(%conn_id, "PendingLane driver starting");
            driver.run().await;
            trace!(%conn_id, "PendingLane driver exited");
        });
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move { driver.run().await });
        C::from_vox_lane(caller, None)
    }

    /// Accept this service lane and proxy all traffic to/from another lane.
    pub fn proxy_to(mut self, other: LaneHandle) {
        let handle = self.take_handle();
        #[cfg(not(target_arch = "wasm32"))]
        tokio::spawn(async move {
            let _ = proxy_lanes(handle, other).await;
        });
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            let _ = proxy_lanes(handle, other).await;
        });
    }

    /// Take the raw LaneHandle for custom use.
    pub fn into_handle(mut self) -> LaneHandle {
        self.take_handle()
    }
}

impl Drop for PendingLane {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let conn_id = handle.lane_id();
            warn!(%conn_id, "PendingLane dropped without being consumed — closing service lane");
            if let Some(tx) = handle.control_tx.as_ref() {
                let _ = send_drop_control(tx, DropControlRequest::Close(conn_id));
            }
        }
    }
}

// r[impl lane.accept.api]
// r[impl lane.open]
pub trait LaneAcceptor: MaybeSend + MaybeSync + 'static {
    fn accept(&self, request: &LaneRequest, connection: PendingLane) -> Result<(), LaneRejection>;
}

/// Any `Handler<DriverReplySink>` is automatically a `LaneAcceptor`.
impl<H> LaneAcceptor for H
where
    H: Handler<crate::DriverReplySink> + Clone + MaybeSend + MaybeSync + 'static,
{
    fn accept(&self, _request: &LaneRequest, connection: PendingLane) -> Result<(), LaneRejection> {
        connection.handle_with(self.clone());
        Ok(())
    }
}

/// Wrapper that turns a closure into a `LaneAcceptor`.
pub struct LaneAcceptorFn<F>(pub F);

impl<F> LaneAcceptor for LaneAcceptorFn<F>
where
    F: Fn(&LaneRequest, PendingLane) -> Result<(), LaneRejection> + MaybeSend + MaybeSync + 'static,
{
    fn accept(&self, request: &LaneRequest, connection: PendingLane) -> Result<(), LaneRejection> {
        (self.0)(request, connection)
    }
}

/// Create a `LaneAcceptor` from a closure.
pub fn lane_acceptor_fn<F>(f: F) -> LaneAcceptorFn<F>
where
    F: Fn(&LaneRequest, PendingLane) -> Result<(), LaneRejection> + MaybeSend + MaybeSync + 'static,
{
    LaneAcceptorFn(f)
}

// ---------------------------------------------------------------------------
// Open/close request types (from ConnectionHandle → run loop)
// ---------------------------------------------------------------------------

struct OpenRequest {
    settings: ConnectionSettings,
    metadata: Metadata,
    result_tx: vox_rt::sync::oneshot::Sender<Result<LaneHandle, ConnectionError>>,
}

struct CloseRequest {
    conn_id: LaneId,
    metadata: Metadata,
    result_tx: vox_rt::sync::oneshot::Sender<Result<(), ConnectionError>>,
}

#[derive(Debug, Clone)]
pub(crate) enum DropControlRequest {
    Shutdown {
        deadline: Instant,
    },
    ResourceClose,
    Close(LaneId),
    ProtocolClose {
        conn_id: LaneId,
        description: String,
    },
}

impl DropControlRequest {
    /// Capture the budget before enqueueing, not when the control pump or
    /// connection loop eventually gets scheduled.
    fn shutdown() -> Self {
        Self::Shutdown {
            deadline: Instant::now() + Duration::from_secs(1),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailureDisposition {
    Cancelled,
    Indeterminate,
    /// A reply may already have reached the transport; do not send a replacement.
    TransportUncertain,
}

/// Identity of one handler attempt, independent of a reusable wire request ID.
#[derive(Clone, Debug)]
pub(crate) struct HandlerIdentity(Arc<()>);

impl HandlerIdentity {
    pub(crate) fn new() -> Self {
        Self(Arc::new(()))
    }
}

impl PartialEq for HandlerIdentity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for HandlerIdentity {}
impl std::hash::Hash for HandlerIdentity {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&(Arc::as_ptr(&self.0) as usize), state);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FailureNotice {
    pub(crate) request_id: RequestId,
    pub(crate) disposition: FailureDisposition,
    pub(crate) identity: HandlerIdentity,
}

#[cfg(not(target_arch = "wasm32"))]
fn send_drop_control(
    tx: &mpsc::UnboundedSender<DropControlRequest>,
    req: DropControlRequest,
) -> Result<(), ()> {
    tx.send(req).map_err(|_| ())
}

#[cfg(target_arch = "wasm32")]
fn send_drop_control(
    tx: &mpsc::UnboundedSender<DropControlRequest>,
    req: DropControlRequest,
) -> Result<(), ()> {
    tx.try_send(req).map_err(|_| ())
}

// ---------------------------------------------------------------------------
// ConnectionHandle — cloneable handle for opening/closing service lanes
// ---------------------------------------------------------------------------

/// Cloneable handle for opening and closing service lanes.
///
/// The connection's `run()` loop must be running concurrently for lane-open
/// requests and RPC traffic to be processed.
// r[impl connection.model]
// r[impl connection.lifecycle.driven]
// r[impl lane.open.api]
#[derive(Clone)]
pub struct ConnectionHandle {
    open_tx: mpsc::Sender<OpenRequest>,
    close_tx: mpsc::Sender<CloseRequest>,
    control_tx: mpsc::UnboundedSender<DropControlRequest>,
    peer_identity: PeerIdentity,
    peer_evidence: PeerEvidence,
    _control_caller: Option<crate::Caller>,
}

impl ConnectionHandle {
    /// The immutable identity resolved for the peer at connection establishment.
    // r[impl connection.identity]
    // r[impl connection.identity.scope]
    pub fn peer_identity(&self) -> &PeerIdentity {
        &self.peer_identity
    }

    /// Locally asserted evidence used to resolve the peer identity.
    // r[impl connection.evidence]
    pub fn peer_evidence(&self) -> &PeerEvidence {
        &self.peer_evidence
    }

    /// Resolve when the connection's private control lane closes.
    // r[impl lane.control]
    pub async fn closed(&self) {
        if let Some(caller) = &self._control_caller {
            caller.closed().await;
        }
    }

    /// Open a typed service lane on this connection using default lane limits.
    ///
    /// Sends `vox-service` metadata automatically from the client's
    /// `SERVICE_NAME`. Creates a `Driver` and spawns it, returning
    /// a ready-to-use typed client.
    pub async fn open_lane<Client: crate::FromVoxLane>(&self) -> Result<Client, ConnectionError> {
        self.open_lane_with_settings(ConnectionSettings {
            parity: Parity::Odd,
            max_concurrent_requests: 64,
            initial_channel_credit: vox_types::DEFAULT_INITIAL_CHANNEL_CREDIT,
        })
        .await
    }

    /// Open a typed service lane with explicit lane settings.
    pub async fn open_lane_with_settings<Client: crate::FromVoxLane>(
        &self,
        settings: ConnectionSettings,
    ) -> Result<Client, ConnectionError> {
        use crate::{Caller, Driver};

        let metadata = vox_types::metadata()
            .str(
                crate::connection::builders::VOX_SERVICE_METADATA_KEY,
                Client::SERVICE_NAME,
            )
            .build();
        let handle = self.open_lane_handle(settings, metadata).await?;
        let mut driver = Driver::new(handle, ());
        let caller = Caller::new(driver.caller());
        #[cfg(not(target_arch = "wasm32"))]
        tokio::spawn(async move { driver.run().await });
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move { driver.run().await });
        Ok(Client::from_vox_lane(caller, Some(self.clone())))
    }

    /// Open a raw service lane on this connection.
    ///
    /// Allocates a lane ID, sends `LaneOpen` to the peer, and waits for
    /// `LaneAccept` or `LaneReject`. The connection's `run()` loop processes
    /// the response and completes the returned future.
    // r[impl lane.open.wire]
    // r[impl lane.open]
    // r[impl lane.wire]
    pub async fn open_lane_handle(
        &self,
        settings: ConnectionSettings,
        metadata: Metadata,
    ) -> Result<LaneHandle, ConnectionError> {
        let (result_tx, result_rx) = vox_rt::sync::oneshot::channel("connection.open_result");
        self.open_tx
            .send(OpenRequest {
                settings,
                metadata,
                result_tx,
            })
            .await
            .map_err(|_| ConnectionError::Protocol("connection closed".into()))?;
        result_rx
            .await
            .map_err(|_| ConnectionError::Protocol("connection closed".into()))?
    }

    /// Close an open service lane.
    ///
    /// Sends `LaneClose` to the peer and removes the lane slot. After this
    /// returns, no further messages will be routed to the lane's driver.
    // r[impl lane.close]
    // r[impl lane.wire]
    pub async fn close_lane(
        &self,
        lane_id: LaneId,
        metadata: Metadata,
    ) -> Result<(), ConnectionError> {
        let (result_tx, result_rx) = vox_rt::sync::oneshot::channel("connection.close_result");
        self.close_tx
            .send(CloseRequest {
                conn_id: lane_id,
                metadata,
                result_tx,
            })
            .await
            .map_err(|_| ConnectionError::Protocol("connection closed".into()))?;
        result_rx
            .await
            .map_err(|_| ConnectionError::Protocol("connection closed".into()))?
    }

    /// Request shutdown of the entire connection and all lanes.
    // r[impl connection.shutdown.explicit]
    pub fn shutdown(&self) -> Result<(), ConnectionError> {
        send_drop_control(&self.control_tx, DropControlRequest::shutdown())
            .map_err(|_| ConnectionError::Protocol("connection closed".into()))
    }
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// Connection state machine.
// r[impl connection.model]
// r[impl connection.lifecycle.driven]
// r[impl connection.protocol]
// r[impl lane]
// r[impl lane.control]
// r[impl lane.service]
pub struct Connection {
    /// Conduit receiver
    rx: Box<dyn DynConduitRx>,

    // r[impl connection.role]
    role: ConnectionRole,

    /// Our local parity — determines which service lane IDs we allocate.
    // r[impl connection.lane-id-parity]
    parity: Parity,

    /// Shared core (for sending) — also held by all ConnectionSenders.
    connection_core: Arc<ConnectionCore>,
    local_connection_settings: ConnectionSettings,
    peer_connection_settings: Option<ConnectionSettings>,
    peer_identity: PeerIdentity,
    peer_evidence: PeerEvidence,
    resource_limits: Option<ConnectionResourceLimits>,
    peer_lane_opens: usize,
    resource_exhausted: bool,
    outbound_abort: futures_util::future::AbortHandle,
    _control_pump: ControlPumpGuard,

    /// Service lane state (active, pending inbound, pending outbound).
    conns: BTreeMap<LaneId, ConnectionSlot>,
    /// Allocator for outbound service lane IDs (uses connection parity).
    conn_ids: IdAllocator<LaneId>,

    /// Callback for accepting inbound service lanes.
    lane_acceptor: Option<Arc<dyn LaneAcceptor>>,

    /// Receiver for open requests from ConnectionHandle.
    open_rx: mpsc::Receiver<OpenRequest>,

    /// Receiver for close requests from ConnectionHandle.
    close_rx: mpsc::Receiver<CloseRequest>,

    /// Sender/receiver for explicit connection and lane control requests.
    control_tx: mpsc::UnboundedSender<DropControlRequest>,
    control_rx: mpsc::UnboundedReceiver<DropControlRequest>,

    /// Optional proactive keepalive runtime config for connection ID 0.
    keepalive: Option<ConnectionKeepaliveConfig>,

    observer: Option<VoxObserverHandle>,
}

#[derive(Debug)]
struct KeepaliveRuntime {
    ping_interval: Duration,
    pong_timeout: Duration,
    next_ping_at: vox_types::time::tokio::Instant,
    waiting_pong_nonce: Option<u64>,
    pong_deadline: vox_types::time::tokio::Instant,
    next_ping_nonce: u64,
}

// r[impl lane.id]
/// Static data for one active lane.
#[derive(Debug)]
pub struct LaneState {
    /// Unique lane identifier.
    pub id: LaneId,

    /// Our settings
    pub local_settings: ConnectionSettings,

    /// The peer's settings
    pub peer_settings: ConnectionSettings,

    /// Sender for routing incoming messages to the per-lane driver task.
    conn_tx: mpsc::Sender<RecvMessage>,
    closed_tx: watch::Sender<Option<ConnectionCloseReason>>,
    generation: Arc<LaneLifetime>,

    /// Per-lane schema recv tracker.
    schema_recv_tracker: Arc<vox_types::SchemaRecvTracker>,
    lane_grant: Arc<Mutex<LaneGrant>>,
    peer_channel_ids: HashSet<vox_types::ChannelId>,
    received_schema_count: usize,
    received_schema_bytes: usize,
    peer_calls: usize,
}

#[derive(Debug)]
enum ConnectionSlot {
    Active(LaneState),
    PendingOutbound(PendingOutboundData),
    /// Retains an old generation while its write or lock outcome is unknown.
    /// No later lane may claim this numeric ID in this connection.
    Retiring(Arc<LaneLifetime>),
}

/// Owns best-effort cleanup even when `run` is dropped before its first poll.
/// Abort and detachment are requests, not proof that transport writers stopped.
struct ControlPumpGuard {
    pump: futures_util::future::AbortHandle,
    worker: futures_util::future::AbortHandle,
    core: Arc<ConnectionCore>,
    #[cfg(not(target_arch = "wasm32"))]
    owner_signal: Option<watch::Sender<()>>,
}

impl Drop for ControlPumpGuard {
    fn drop(&mut self) {
        self.pump.abort();
        self.worker.abort();
        self.core
            .admission_stopped
            .store(true, std::sync::atomic::Ordering::Release);
        // Closing this independent channel wakes waiters that registered
        // before owner drop, even though the core itself can outlive us.
        // A panicking custom waker is contained; complete wakeup is not
        // guaranteed for a waker that panics or blocks.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let signal = self.owner_signal.take();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(signal)));
        }
        // Do not wait on a synchronous lock during destructor cleanup. A busy
        // or poisoned lock leaves teardown uncertain and may retain the sender.
        let tx = self
            .core
            .inner
            .try_lock()
            .ok()
            .and_then(|mut inner| inner.tx.take());
        if let Some(tx) = tx {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tx.abort()));
        }
    }
}

/// Requests abort on every unsuccessful or cancelled shutdown path. Abort may
/// be unsupported or asynchronous; this guard does not prove teardown.
struct ShutdownAbortGuard {
    core: Arc<ConnectionCore>,
    tx: Arc<dyn DynConduitTx>,
    worker: futures_util::future::AbortHandle,
    armed: bool,
}

impl Drop for ShutdownAbortGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.worker.abort();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.tx.abort()));
        // Detach only our exact sender, without waiting on a possibly held
        // synchronous mutex. Other clones may still exist after this request.
        if let Ok(mut inner) = self.core.inner.try_lock()
            && inner
                .tx
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &self.tx))
        {
            inner.tx.take();
        }
    }
}

/// Debug-printable wrapper that omits the oneshot sender.
struct PendingOutboundData {
    local_settings: ConnectionSettings,
    establishment_started_at: Instant,
    result_tx: Option<vox_rt::sync::oneshot::Sender<Result<LaneHandle, ConnectionError>>>,
}

impl std::fmt::Debug for PendingOutboundData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingOutbound")
            .field("local_settings", &self.local_settings)
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct ConnectionSender {
    lane_id: LaneId,
    pub(crate) connection_core: Arc<ConnectionCore>,
    failures: Arc<mpsc::UnboundedSender<FailureNotice>>,
    lane_closed: watch::Receiver<Option<ConnectionCloseReason>>,
    lane_generation: Arc<LaneLifetime>,
    response_book: response_reservation::RequestBook,
}

fn forwarded_payload<'a>(payload: &'a vox_types::Payload<'a>) -> vox_types::Payload<'a> {
    let vox_types::Payload::Encoded(bytes) = payload else {
        unreachable!("proxy forwarding expects decoded incoming payload bytes")
    };
    vox_types::Payload::Encoded(bytes)
}

fn forwarded_request_body<'a>(body: &'a RequestBody<'a>) -> RequestBody<'a> {
    match body {
        RequestBody::Call(call) => RequestBody::Call(vox_types::RequestCall {
            method_id: call.method_id,
            channels: call.channels.clone(),
            metadata: call.metadata.clone(),
            args: forwarded_payload(&call.args),
            schemas: call.schemas.clone(),
        }),
        RequestBody::Response(response) => RequestBody::Response(RequestResponse {
            metadata: response.metadata.clone(),
            ret: forwarded_payload(&response.ret),
            schemas: response.schemas.clone(),
        }),
        RequestBody::Cancel(cancel) => RequestBody::Cancel(vox_types::RequestCancel {
            metadata: cancel.metadata.clone(),
        }),
    }
}

/// Swap a `Call`'s args to already-encoded `bytes`, narrowing the message
/// lifetime to that of `bytes` (`Message` is covariant in its lifetime). Used by
/// the out-of-band channel send path, where the args were pre-encoded into a
/// local buffer that outlives the synchronous `prepare_msg`.
fn swap_call_args_to_bytes<'s>(mut msg: Message<'s>, bytes: &'s [u8]) -> Message<'s> {
    if let MessagePayload::RequestMessage(req) = &mut msg.payload
        && let RequestBody::Call(call) = &mut req.body
    {
        call.args = vox_types::Payload::Encoded(bytes);
    }
    msg
}

fn forwarded_channel_body<'a>(body: &'a vox_types::ChannelBody<'a>) -> vox_types::ChannelBody<'a> {
    match body {
        vox_types::ChannelBody::Item(item) => {
            vox_types::ChannelBody::Item(vox_types::ChannelItem {
                item: forwarded_payload(&item.item),
            })
        }
        vox_types::ChannelBody::Close(close) => {
            vox_types::ChannelBody::Close(vox_types::ChannelClose {
                metadata: close.metadata.clone(),
            })
        }
        vox_types::ChannelBody::Reset(reset) => {
            vox_types::ChannelBody::Reset(vox_types::ChannelReset {
                metadata: reset.metadata.clone(),
            })
        }
        vox_types::ChannelBody::GrantCredit(credit) => {
            vox_types::ChannelBody::GrantCredit(vox_types::ChannelGrantCredit {
                additional: credit.additional,
            })
        }
    }
}

impl ConnectionSender {
    pub(crate) fn lane_id(&self) -> LaneId {
        self.lane_id
    }

    pub(crate) fn lane_generation(&self) -> Arc<LaneLifetime> {
        self.lane_generation.clone()
    }

    pub(crate) async fn send_with_binder<'a>(
        &self,
        msg: ConnectionMessage<'a>,
        binder: Option<&'a dyn vox_types::ChannelBinder>,
    ) -> Result<(), ()> {
        self.send_with_binder_and_method(msg, binder, None).await
    }

    pub(crate) async fn send_with_binder_and_method<'a>(
        &self,
        msg: ConnectionMessage<'a>,
        binder: Option<&'a dyn vox_types::ChannelBinder>,
        channel_method: Option<&'static vox_types::MethodDescriptor>,
    ) -> Result<(), ()> {
        self.send_with_binder_and_method_observing_channels(msg, binder, channel_method, |_| {})
            .await
    }

    pub(crate) async fn send_with_binder_and_method_observing_channels<'a>(
        &self,
        msg: ConnectionMessage<'a>,
        binder: Option<&'a dyn vox_types::ChannelBinder>,
        channel_method: Option<&'static vox_types::MethodDescriptor>,
        declared_channels: impl FnOnce(&[vox_types::ChannelId]),
    ) -> Result<(), ()> {
        // This sender retains its original lane's close signal even if the
        // numeric lane ID is later reused. Admission/worker races still require
        // the separately planned lifetime capability and teardown handshake.
        if self.lane_closed.borrow().is_some() {
            declared_channels(&[]);
            return Err(());
        }
        let payload = match msg {
            ConnectionMessage::Request(r) => MessagePayload::RequestMessage(r),
            ConnectionMessage::Channel(c) => MessagePayload::ChannelMessage(c),
            ConnectionMessage::Schema(s) => MessagePayload::SchemaMessage(s),
        };
        let message = Message {
            lane_id: self.lane_id,
            payload,
        };
        self.connection_core
            .send_with_options(
                message,
                binder,
                None,
                channel_method,
                Vec::new(),
                declared_channels,
                Some(self.lane_closed.clone()),
                Some(self.lane_generation.clone()),
                None,
            )
            .await
            .map_err(|_| ())
    }

    pub(crate) async fn send_channel_with_writer_schema<'a>(
        &self,
        channel: ChannelMessage<'a>,
        writer_schema: Option<vox_types::ChannelWriterSchemaPlan>,
    ) -> Result<(), ()> {
        if self.lane_closed.borrow().is_some() {
            return Err(());
        }
        let extra_schema_sends = writer_schema
            .map(PendingSchemaSend::from)
            .into_iter()
            .collect();
        self.connection_core
            .send_with_options(
                Message {
                    lane_id: self.lane_id,
                    payload: MessagePayload::ChannelMessage(channel),
                },
                None,
                None,
                None,
                extra_schema_sends,
                |_| {},
                Some(self.lane_closed.clone()),
                Some(self.lane_generation.clone()),
                None,
            )
            .await
            .map_err(|_| ())
    }

    /// Send an arbitrary connection message
    pub async fn send<'a>(&self, msg: ConnectionMessage<'a>) -> Result<(), ()> {
        self.send_with_binder(msg, None).await
    }

    pub(crate) fn try_send_channel_with_writer_schema<'a>(
        &self,
        channel: ChannelMessage<'a>,
        writer_schema: Option<vox_types::ChannelWriterSchemaPlan>,
    ) -> Result<(), TrySendError<()>> {
        if self.lane_closed.borrow().is_some() {
            return Err(TrySendError::Closed(()));
        }
        let extra_schema_sends = writer_schema
            .map(PendingSchemaSend::from)
            .into_iter()
            .collect();
        self.connection_core.try_send_with_options(
            Message {
                lane_id: self.lane_id,
                payload: MessagePayload::ChannelMessage(channel),
            },
            None,
            None,
            None,
            extra_schema_sends,
            Some(self.lane_closed.clone()),
            Some(self.lane_generation.clone()),
        )
    }

    /// Send a received connection message without re-materializing payload values.
    pub(crate) async fn send_owned(
        &self,
        schemas: Arc<vox_types::SchemaRecvTracker>,
        msg: SelfRef<ConnectionMessage<'static>>,
    ) -> Result<(), ()> {
        self.send_owned_with_identity(schemas, msg, None, None)
            .await
    }

    /// Forward a request with a rewritten wire ID and, for responses, its
    /// original ingress reservation. Other messages cannot carry either.
    async fn send_owned_with_identity(
        &self,
        schemas: Arc<vox_types::SchemaRecvTracker>,
        msg: SelfRef<ConnectionMessage<'static>>,
        request_id: Option<RequestId>,
        reservation: Option<response_reservation::ResponseReservation>,
    ) -> Result<(), ()> {
        if self.lane_closed.borrow().is_some() {
            return Err(());
        }
        let msg_ref = msg.get();
        if request_id.is_some() && !matches!(msg_ref, ConnectionMessage::Request(_)) {
            return Err(());
        }
        if reservation.is_some()
            && !matches!(
                msg_ref,
                ConnectionMessage::Request(RequestMessage {
                    body: RequestBody::Response(_),
                    ..
                })
            )
        {
            return Err(());
        }
        let payload = match msg_ref {
            ConnectionMessage::Request(request) => MessagePayload::RequestMessage(RequestMessage {
                id: request_id.unwrap_or(request.id),
                body: forwarded_request_body(&request.body),
            }),
            ConnectionMessage::Channel(channel) => MessagePayload::ChannelMessage(ChannelMessage {
                id: channel.id,
                body: forwarded_channel_body(&channel.body),
            }),
            ConnectionMessage::Schema(schema) => MessagePayload::SchemaMessage(SchemaMessage {
                method_id: schema.method_id,
                direction: schema.direction,
                schemas: schema.schemas.clone(),
            }),
        };

        self.connection_core
            .send_with_options(
                Message {
                    lane_id: self.lane_id,
                    payload,
                },
                None,
                Some(&*schemas),
                None,
                Vec::new(),
                |_| {},
                Some(self.lane_closed.clone()),
                Some(self.lane_generation.clone()),
                reservation,
            )
            .await
            .map_err(|_| ())
    }

    /// Send a response specifically
    pub async fn send_response<'a>(
        &self,
        request_id: RequestId,
        response: RequestResponse<'a>,
    ) -> Result<(), ()> {
        self.send(ConnectionMessage::Request(RequestMessage {
            id: request_id,
            body: RequestBody::Response(response),
        }))
        .await
    }

    /// Shape a response using an explicit method ID, then send it.
    pub async fn send_response_for_method<'a>(
        &self,
        request_id: RequestId,
        method_id: vox_types::MethodId,
        mut response: RequestResponse<'a>,
    ) -> Result<(), ()> {
        self.prepare_response_for_method(request_id, method_id, &mut response);
        self.send(ConnectionMessage::Request(RequestMessage {
            id: request_id,
            body: RequestBody::Response(response),
        }))
        .await
    }

    /// Experimental identity-bearing driver reply path. The guarded proxy
    /// uses its own forwarding path; legacy transports remain unqualified.
    async fn send_reserved_response<'a>(
        &self,
        request_id: RequestId,
        response: RequestResponse<'a>,
        reservation: response_reservation::ResponseReservation,
    ) -> Result<(), ()> {
        self.connection_core
            .send_with_options(
                Message {
                    lane_id: self.lane_id,
                    payload: MessagePayload::RequestMessage(RequestMessage {
                        id: request_id,
                        body: RequestBody::Response(response),
                    }),
                },
                None,
                None,
                None,
                Vec::new(),
                |_| {},
                Some(self.lane_closed.clone()),
                Some(self.lane_generation.clone()),
                Some(reservation),
            )
            .await
    }

    pub(crate) fn record_response_capability(
        &self,
        request_id: RequestId,
        method_id: vox_types::MethodId,
    ) -> Result<Option<response_reservation::RequestCapability>, ()> {
        if self.lane_closed.borrow().is_some() {
            return Err(());
        }
        if !self.connection_core.guarded_responses {
            return Ok(None);
        }
        self.response_book
            .record(request_id, method_id)
            .map(Some)
            .map_err(|_| ())
    }

    pub(crate) async fn send_capability_response<'a>(
        &self,
        request_id: RequestId,
        method_id: vox_types::MethodId,
        response: RequestResponse<'a>,
        capability: response_reservation::RequestCapability,
    ) -> Result<(), ()> {
        if capability.request_id() != request_id {
            return Err(());
        }
        let (recorded_method, reservation) =
            self.response_book.reserve(&capability).map_err(|_| ())?;
        if recorded_method != method_id {
            return Err(());
        }
        self.send_reserved_response(request_id, response, reservation)
            .await
    }

    /// Shape a response using an explicit method ID without sending it yet.
    pub(crate) fn prepare_response_for_method(
        &self,
        request_id: RequestId,
        method_id: vox_types::MethodId,
        response: &mut RequestResponse<'_>,
    ) {
        self.connection_core.prepare_response_for_method(
            self.lane_id,
            request_id,
            method_id,
            response,
        );
    }

    /// Attach the method's response schema for an explicit wire `shape`. Used when the
    /// driver synthesizes an error response whose payload is an erased `Result` but
    /// which must advertise the method's real response schema so the caller can decode.
    pub(crate) fn prepare_response_for_shape(
        &self,
        request_id: RequestId,
        method_id: vox_types::MethodId,
        shape: &'static Shape,
        response: &mut RequestResponse<'_>,
    ) {
        self.connection_core.prepare_response_for_shape(
            self.lane_id,
            request_id,
            method_id,
            shape,
            response,
        );
    }

    /// Mark a request as failed by removing any pending response slot.
    /// Called when a send error occurs or no reply was sent.
    pub(crate) fn mark_failure(
        &self,
        request_id: RequestId,
        disposition: FailureDisposition,
        identity: HandlerIdentity,
    ) {
        let _ = self.failures.send(FailureNotice {
            request_id,
            disposition,
            identity,
        });
    }
}

pub struct LaneHandle {
    pub(crate) sender: ConnectionSender,
    pub(crate) rx: mpsc::Receiver<RecvMessage>,
    pub(crate) failures_rx: mpsc::UnboundedReceiver<FailureNotice>,
    pub(crate) control_tx: Option<mpsc::UnboundedSender<DropControlRequest>>,
    pub(crate) closed_rx: watch::Receiver<Option<ConnectionCloseReason>>,
    pub(crate) local_settings: ConnectionSettings,
    pub(crate) peer_settings: ConnectionSettings,
    pub(crate) max_peer_channel_credit: Option<u32>,
    /// The parity this side should use for allocating request/channel IDs.
    pub parity: Parity,
    pub(crate) observer: Option<VoxObserverHandle>,
    pub(crate) peer_identity: PeerIdentity,
    pub(crate) peer_evidence: PeerEvidence,
    pub(crate) lane_grant: LaneGrant,
    pub(crate) lane_grant_state: Arc<Mutex<LaneGrant>>,
}

impl std::fmt::Debug for LaneHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaneHandle")
            .field("lane_id", &self.sender.lane_id)
            .finish()
    }
}

pub(crate) enum ConnectionMessage<'payload> {
    Request(RequestMessage<'payload>),
    Channel(ChannelMessage<'payload>),
    Schema(SchemaMessage),
}

vox_types::impl_reborrow!(ConnectionMessage);

/// A message routed to a driver, carrying the `SchemaRecvTracker` that was
/// current when the connection runtime received it. This ensures each message uses the
/// correct tracker even across reconnections.
pub(crate) struct RecvMessage {
    pub schemas: Arc<vox_types::SchemaRecvTracker>,
    pub msg: SelfRef<ConnectionMessage<'static>>,
    /// Descriptors that arrived with this frame (`SCM_RIGHTS`). Threaded to
    /// the typed-decode site; `()` off-Unix.
    pub fds: vox_types::FrameFds,
}

impl LaneHandle {
    fn set_lane_grant(&mut self, grant: LaneGrant) {
        *self
            .lane_grant_state
            .lock()
            .expect("lane grant state mutex poisoned") = grant.clone();
        self.lane_grant = grant;
    }

    /// Returns the lane ID for this handle.
    pub fn lane_id(&self) -> LaneId {
        self.sender.lane_id
    }

    /// The immutable peer identity resolved during connection establishment.
    // r[impl request.authorization]
    pub fn peer_identity(&self) -> &PeerIdentity {
        &self.peer_identity
    }

    /// Locally asserted peer evidence available to this lane.
    // r[impl request.authorization]
    pub fn peer_evidence(&self) -> &PeerEvidence {
        &self.peer_evidence
    }

    /// The local lane grant associated with this lane.
    // r[impl lane.authorization.context]
    // r[impl request.authorization]
    pub fn lane_grant(&self) -> &LaneGrant {
        &self.lane_grant
    }

    /// Resolve when this lane closes.
    pub async fn closed(&self) {
        if self.closed_rx.borrow().is_some() {
            return;
        }
        let mut rx = self.closed_rx.clone();
        while rx.changed().await.is_ok() {
            if rx.borrow().is_some() {
                return;
            }
        }
    }

    /// Return whether this lane is still considered connected.
    pub fn is_connected(&self) -> bool {
        self.closed_rx.borrow().is_none()
    }

    pub fn close_reason(&self) -> Option<ConnectionCloseReason> {
        *self.closed_rx.borrow()
    }

    // r[impl rpc.debug.snapshot]
    pub fn debug_snapshot(&self) -> VoxDebugSnapshot {
        let (outbound_queue_depth, outbound_queue_capacity) =
            self.sender.connection_core.outbound_queue_stats();
        VoxDebugSnapshot {
            lanes: vec![LaneDebugSnapshot {
                lane_id: self.lane_id(),
                endpoint: None,
                surface: None,
                component: None,
                state: if self.closed_rx.borrow().is_some() {
                    LaneDebugState::Closed
                } else {
                    LaneDebugState::Open
                },
                outstanding_requests: 0,
                requests: Vec::new(),
                open_channels: Vec::new(),
                outbound_queue_depth: Some(outbound_queue_depth),
                outbound_queue_capacity: Some(outbound_queue_capacity),
                local_control_queue_depth: None,
                local_control_queue_capacity: None,
                last_inbound_message_at: None,
                last_outbound_message_at: None,
                last_progress_at: None,
                close_reason: *self.closed_rx.borrow(),
                driver_task_status: DriverTaskStatus::Unknown,
            }],
        }
    }

    pub fn dump_debug_snapshot(&self) -> VoxDebugSnapshot {
        let snapshot = self.debug_snapshot();
        tracing::info!(?snapshot, "vox debug snapshot");
        snapshot
    }
}

struct ProxiedRequest {
    destination: RequestId,
    method: vox_types::MethodId,
    capability: response_reservation::RequestCapability,
}

/// One direction of a proxy lane. Destination IDs are never reused in this
/// lane, so an old response cannot be attributed to a later source-ID reuse.
struct ProxiedRequests {
    next: Option<u64>,
    parity: Parity,
    limit: usize,
    by_source: HashMap<RequestId, ProxiedRequest>,
    by_destination: HashMap<RequestId, RequestId>,
}

impl ProxiedRequests {
    fn new(parity: Parity, limit: usize) -> Self {
        Self {
            next: Some(match parity {
                Parity::Odd => 1,
                Parity::Even => 2,
            }),
            parity,
            limit,
            by_source: HashMap::new(),
            by_destination: HashMap::new(),
        }
    }

    fn begin(
        &mut self,
        sender: &ConnectionSender,
        source: RequestId,
        method: vox_types::MethodId,
    ) -> Result<RequestId, ()> {
        if !source.has_parity(self.parity)
            || self.by_source.len() >= self.limit
            || self.by_source.contains_key(&source)
        {
            return Err(());
        }
        let destination = RequestId(self.next.ok_or(())?);
        let capability = sender
            .record_response_capability(source, method)?
            .ok_or(())?;
        self.next = destination.0.checked_add(2);
        self.by_source.insert(
            source,
            ProxiedRequest {
                destination,
                method,
                capability,
            },
        );
        self.by_destination.insert(destination, source);
        Ok(destination)
    }

    fn response(
        &self,
        destination: RequestId,
    ) -> Result<
        (
            RequestId,
            vox_types::MethodId,
            response_reservation::RequestCapability,
        ),
        (),
    > {
        let source = *self.by_destination.get(&destination).ok_or(())?;
        let entry = self.by_source.get(&source).ok_or(())?;
        if entry.destination != destination {
            return Err(());
        }
        Ok((source, entry.method, entry.capability.clone()))
    }

    fn finish(&mut self, destination: RequestId) -> Result<(), ()> {
        let source = self.by_destination.remove(&destination).ok_or(())?;
        let entry = self.by_source.remove(&source).ok_or(())?;
        if entry.destination != destination {
            return Err(());
        }
        Ok(())
    }
}

async fn forward_with_identity(
    recv: RecvMessage,
    source: &ConnectionSender,
    destination: &ConnectionSender,
    outbound: &mut ProxiedRequests,
    inbound: &mut ProxiedRequests,
) -> Result<(), ()> {
    // The existing proxy cannot preserve SCM_RIGHTS. Do not silently discard
    // descriptors on the identity-guarded path.
    if vox_types::frame_fds_len(&recv.fds) != 0 {
        return Err(());
    }
    let operation = match recv.msg.get() {
        ConnectionMessage::Request(request) => match &request.body {
            RequestBody::Call(call) => Some((0, request.id, Some(call.method_id))),
            RequestBody::Response(_) => Some((1, request.id, None)),
            RequestBody::Cancel(_) => Some((2, request.id, None)),
        },
        _ => None,
    };
    match operation {
        Some((0, id, Some(method))) => {
            let rewritten = outbound.begin(source, id, method)?;
            destination
                .send_owned_with_identity(recv.schemas, recv.msg, Some(rewritten), None)
                .await
        }
        Some((1, id, _)) => {
            let (rewritten, method, capability) = inbound.response(id)?;
            let (reserved_method, reservation) = destination
                .response_book
                .reserve(&capability)
                .map_err(|_| ())?;
            if reserved_method != method {
                return Err(());
            }
            destination
                .send_owned_with_identity(
                    recv.schemas,
                    recv.msg,
                    Some(rewritten),
                    Some(reservation),
                )
                .await?;
            inbound.finish(id)
        }
        Some((2, id, _)) => {
            let Some(entry) = outbound.by_source.get(&id) else {
                // A late cancel must not be redirected to a later request.
                return Ok(());
            };
            destination
                .send_owned_with_identity(recv.schemas, recv.msg, Some(entry.destination), None)
                .await
        }
        Some(_) => Err(()),
        None => destination.send_owned(recv.schemas, recv.msg).await,
    }
}

struct ProxyTermination {
    left: Option<mpsc::UnboundedSender<DropControlRequest>>,
    right: Option<mpsc::UnboundedSender<DropControlRequest>>,
    left_lane: LaneId,
    right_lane: LaneId,
    uncertain: bool,
}

impl Drop for ProxyTermination {
    fn drop(&mut self) {
        for (sender, lane) in [(&self.left, self.left_lane), (&self.right, self.right_lane)] {
            if let Some(sender) = sender {
                let request = if self.uncertain || lane.is_control() {
                    DropControlRequest::ResourceClose
                } else {
                    DropControlRequest::Close(lane)
                };
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    send_drop_control(sender, request)
                }));
            }
        }
    }
}

/// Forward all request/channel traffic between two connections.
///
/// On locally settled transports, calls and replies retain request identity
/// through a bounded, parity-correct ID mapping. Other transports retain the
/// legacy forwarding path and do not receive that identity guarantee.
/// It exits when either side closes or a forward send fails, then requests
/// closure of both underlying connections.
pub async fn proxy_lanes(left: LaneHandle, right: LaneHandle) -> Result<(), ConnectionError> {
    if left.parity == right.parity {
        return Err(ConnectionError::Protocol(
            "proxy_lanes requires opposite parities".into(),
        ));
    }
    let left_conn_id = left.lane_id();
    let right_conn_id = right.lane_id();
    let guarded = left.sender.connection_core.guarded_responses
        && right.sender.connection_core.guarded_responses;
    let mut left_to_right = ProxiedRequests::new(
        right.parity,
        left.local_settings
            .max_concurrent_requests
            .min(right.peer_settings.max_concurrent_requests) as usize,
    );
    let mut right_to_left = ProxiedRequests::new(
        left.parity,
        right
            .local_settings
            .max_concurrent_requests
            .min(left.peer_settings.max_concurrent_requests) as usize,
    );
    let LaneHandle {
        sender: left_sender,
        rx: mut left_rx,
        failures_rx: _left_failures_rx,
        control_tx: left_control_tx,
        closed_rx: _left_closed_rx,
        local_settings: _left_local_settings,
        peer_settings: _left_peer_settings,
        max_peer_channel_credit: _left_max_peer_channel_credit,
        parity: _left_parity,
        observer: _left_observer,
        peer_identity: _left_peer_identity,
        peer_evidence: _left_peer_evidence,
        lane_grant: _left_lane_grant,
        lane_grant_state: _left_lane_grant_state,
    } = left;
    let LaneHandle {
        sender: right_sender,
        rx: mut right_rx,
        failures_rx: _right_failures_rx,
        control_tx: right_control_tx,
        closed_rx: _right_closed_rx,
        local_settings: _right_local_settings,
        peer_settings: _right_peer_settings,
        max_peer_channel_credit: _right_max_peer_channel_credit,
        parity: _right_parity,
        observer: _right_observer,
        peer_identity: _right_peer_identity,
        peer_evidence: _right_peer_evidence,
        lane_grant: _right_lane_grant,
        lane_grant_state: _right_lane_grant_state,
    } = right;

    // If forwarding is cancelled while a send is in flight its outcome is
    // uncertain. Terminate both connections rather than reusing a lane or ID.
    let mut termination = ProxyTermination {
        left: left_control_tx,
        right: right_control_tx,
        left_lane: left_conn_id,
        right_lane: right_conn_id,
        uncertain: true,
    };

    let mut failed = false;
    loop {
        enum ProxyEvent {
            Left(Option<RecvMessage>),
            Right(Option<RecvMessage>),
        }

        let event = {
            let left = left_rx.recv().fuse();
            let right = right_rx.recv().fuse();
            futures_util::pin_mut!(left, right);
            futures_util::select_biased! {
                recv = left => ProxyEvent::Left(recv),
                recv = right => ProxyEvent::Right(recv),
            }
        };

        match event {
            ProxyEvent::Left(Some(recv)) => {
                let result = if guarded {
                    forward_with_identity(
                        recv,
                        &left_sender,
                        &right_sender,
                        &mut left_to_right,
                        &mut right_to_left,
                    )
                    .await
                } else {
                    right_sender.send_owned(recv.schemas, recv.msg).await
                };
                if result.is_err() {
                    failed = true;
                    break;
                }
            }
            ProxyEvent::Right(Some(recv)) => {
                let result = if guarded {
                    forward_with_identity(
                        recv,
                        &right_sender,
                        &left_sender,
                        &mut right_to_left,
                        &mut left_to_right,
                    )
                    .await
                } else {
                    left_sender.send_owned(recv.schemas, recv.msg).await
                };
                if result.is_err() {
                    failed = true;
                    break;
                }
            }
            ProxyEvent::Left(None) | ProxyEvent::Right(None) => break,
        }
    }

    // A peer can close while replies are still outstanding. Those requests
    // have no settled response, so they must not be retired as a clean lane.
    termination.uncertain =
        failed || !left_to_right.by_source.is_empty() || !right_to_left.by_source.is_empty();
    if guarded && termination.uncertain {
        Err(ConnectionError::Protocol(
            "guarded proxy forwarding failed or had an uncertain outcome".into(),
        ))
    } else {
        Ok(())
    }
}

/// Errors that can occur during connection establishment or operation.
#[derive(Debug)]
pub enum ConnectionError {
    Io(std::io::Error),
    Protocol(String),
    EstablishmentRejected(vox_types::Decline),
    Rejected(LaneRejection),
    ConnectTimeout,
}

impl ConnectionError {
    /// Returns `true` if a later connection attempt may succeed.
    ///
    /// I/O errors and timeouts are transient — the remote might become available
    /// shortly. Protocol errors and explicit rejections are permanent for this
    /// peer address.
    pub fn is_transient_connect_failure(&self) -> bool {
        matches!(self, Self::Io(_) | Self::ConnectTimeout)
    }
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Protocol(msg) => write!(f, "protocol error: {msg}"),
            Self::EstablishmentRejected(decline) => {
                write!(f, "connection establishment rejected: {}", decline.reason)
            }
            Self::Rejected(rejection) => {
                if let Some(message) = rejection.message() {
                    write!(f, "lane open rejected: {}: {message}", rejection.reason())
                } else {
                    write!(f, "lane open rejected: {}", rejection.reason())
                }
            }
            Self::ConnectTimeout => write!(f, "connect timeout"),
        }
    }
}

impl std::error::Error for ConnectionError {}

fn classify_connection_recv_error(error: &std::io::Error) -> ConnectionCloseReason {
    let message = error.to_string();
    if message.contains("decode error") || message.contains("protocol") {
        ConnectionCloseReason::Protocol
    } else {
        ConnectionCloseReason::Transport
    }
}

fn classify_decode_error(error: &std::io::Error) -> Option<DecodeErrorKind> {
    let message = error.to_string();
    if message.contains("decode error") {
        Some(DecodeErrorKind::Payload)
    } else {
        None
    }
}

impl Connection {
    // r[impl rpc.observability.connection-errors]
    // r[impl rpc.observability.driver]
    fn observe_connection_recv_error(&self, error: &std::io::Error) {
        let Some(observer) = &self.observer else {
            return;
        };

        if let Some(kind) = classify_decode_error(error) {
            for conn_id in self.conns.iter().filter_map(|(conn_id, slot)| {
                matches!(slot, ConnectionSlot::Active(_)).then_some(*conn_id)
            }) {
                observer.driver_event(vox_types::DriverEvent::DecodeError {
                    lane_id: conn_id,
                    kind,
                });
            }
            return;
        }

        observer.transport_event(vox_types::TransportEvent::Closed {
            lane_id: None,
            reason: classify_connection_recv_error(error),
        });
    }

    fn close_connection_for_protocol_error(
        &mut self,
        conn_id: LaneId,
        detail: impl std::fmt::Display,
    ) {
        warn!(%conn_id, "closing connection after protocol error: {detail}");
        self.remove_connection_with_reason(&conn_id, ConnectionCloseReason::Protocol);
    }

    fn record_received_schema_bytes(
        &mut self,
        _conn_id: LaneId,
        schema_recv_tracker: Arc<vox_types::SchemaRecvTracker>,
        method_id: vox_types::MethodId,
        direction: vox_types::BindingDirection,
        schema_bytes: &vox_types::SchemaBytes,
        _context: &str,
    ) -> bool {
        // The `schemas` field carries the peer's phon self-describing schema closure.
        // Store it verbatim; recording is best-effort/idempotent (`r[schema.exchange]`),
        // so a duplicate binding is not a protocol error.
        schema_recv_tracker.record_received(method_id, direction, schema_bytes.0.clone());
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn pre_handshake<Tx, Rx>(
        tx: Tx,
        rx: Rx,
        lane_acceptor: Option<Arc<dyn LaneAcceptor>>,
        open_rx: mpsc::Receiver<OpenRequest>,
        close_rx: mpsc::Receiver<CloseRequest>,
        control_tx: mpsc::UnboundedSender<DropControlRequest>,
        control_rx: mpsc::UnboundedReceiver<DropControlRequest>,
        keepalive: Option<ConnectionKeepaliveConfig>,
        observer: Option<VoxObserverHandle>,
    ) -> Self
    where
        Tx: ConduitTx<Msg = MessageFamily> + MaybeSend + MaybeSync + 'static,
        Rx: ConduitRx<Msg = MessageFamily> + MaybeSend + 'static,
    {
        let (outbound_tx, outbound_rx) = mpsc::channel("connection.outbound", 256);
        let (admission_stopped_tx, admission_stopped_rx) = watch::channel(false);
        #[cfg(not(target_arch = "wasm32"))]
        let (owner_signal, owner_closed) = watch::channel(());
        let guarded_responses = ConduitTx::send_success_is_locally_settled(&tx);
        let connection_core = Arc::new(ConnectionCore {
            inner: std::sync::Mutex::new(ConnectionCoreInner {
                tx: Some(Arc::new(tx) as Arc<dyn DynConduitTx>),
                conns: HashMap::new(),
            }),
            outbound_tx,
            admission: vox_rt::sync::Mutex::new("connection.outbound_admission", false),
            admission_stopped: std::sync::atomic::AtomicBool::new(false),
            guarded_responses,
            admission_stopped_tx,
            _admission_stopped_rx: admission_stopped_rx,
            admission_waiters: wake_registry::WakeRegistry::default(),
            #[cfg(not(target_arch = "wasm32"))]
            owner_closed,
            #[cfg(test)]
            before_admission_commit: std::sync::Mutex::new(None),
            #[cfg(test)]
            before_worker_write: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(test)]
            injected_admission_notification_failures: std::sync::atomic::AtomicUsize::new(0),
            observer: observer.clone(),
            channel_gates: std::sync::Mutex::new(HashMap::new()),
        });
        let outbound_abort = spawn_outbound_worker(outbound_rx, control_tx.clone());
        let (dispatch_tx, dispatch_rx) = mpsc::unbounded_channel("connection.control.dispatch");
        let control_pump = ControlPumpGuard {
            pump: spawn_control_pump(control_rx, dispatch_tx, Arc::downgrade(&connection_core)),
            worker: outbound_abort.clone(),
            core: connection_core.clone(),
            #[cfg(not(target_arch = "wasm32"))]
            owner_signal: Some(owner_signal),
        };
        Connection {
            rx: Box::new(rx),
            role: ConnectionRole::Initiator, // overwritten in establish_as_*
            parity: Parity::Odd,             // overwritten in establish_as_*
            connection_core,
            local_connection_settings: ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            peer_connection_settings: None,
            peer_identity: PeerIdentity::anonymous(),
            peer_evidence: PeerEvidence::none(),
            resource_limits: None,
            peer_lane_opens: 0,
            resource_exhausted: false,
            outbound_abort,
            _control_pump: control_pump,
            conns: BTreeMap::new(),
            conn_ids: IdAllocator::new(Parity::Odd), // overwritten in establish_as_*
            lane_acceptor,
            open_rx,
            close_rx,
            control_tx,
            control_rx: dispatch_rx,
            keepalive,
            observer,
        }
    }

    // r[impl connection.handshake]
    fn establish_from_handshake(
        &mut self,
        result: HandshakeResult,
    ) -> Result<LaneHandle, ConnectionError> {
        self.role = result.role;
        self.parity = result.our_settings.parity;
        self.conn_ids = IdAllocator::new(result.our_settings.parity);
        self.local_connection_settings = result.our_settings.clone();
        self.peer_connection_settings = Some(result.peer_settings.clone());
        self.peer_identity = result.peer_identity.clone();
        self.peer_evidence = result.peer_evidence.clone();

        self.try_make_connection_handle(LaneId::CONTROL, result.our_settings, result.peer_settings)
    }

    #[cfg(test)]
    fn make_connection_handle(
        &mut self,
        conn_id: LaneId,
        local_settings: ConnectionSettings,
        peer_settings: ConnectionSettings,
    ) -> LaneHandle {
        self.try_make_connection_handle(conn_id, local_settings, peer_settings)
            .expect("test lane must be available")
    }

    fn try_make_connection_handle(
        &mut self,
        conn_id: LaneId,
        local_settings: ConnectionSettings,
        peer_settings: ConnectionSettings,
    ) -> Result<LaneHandle, ConnectionError> {
        if self.resource_exhausted || self.conns.contains_key(&conn_id) {
            return Err(ConnectionError::Protocol(
                "lane id is unavailable or connection retirement is unconfirmed".into(),
            ));
        }
        // A numeric ID is not a lane generation. Old queued batches retain
        // their own Arc; a newly activated lane must get fresh schema and
        // request bookkeeping even when the wire ID is reused.
        {
            let mut inner = self.connection_core.inner.try_lock().map_err(|_| {
                ConnectionError::Protocol("lane send-state lock is unavailable".into())
            })?;
            inner.conns.insert(
                conn_id,
                Arc::new(std::sync::Mutex::new(SendConnState::new())),
            );
        }
        let label = format!("connection.lane{}", conn_id.0);
        let (conn_tx, conn_rx) = mpsc::channel::<RecvMessage>(&label, 64);
        let (failures_tx, failures_rx) = mpsc::unbounded_channel(format!("{label}.failures"));
        let (closed_tx, closed_rx) = watch::channel(None);
        let lane_generation = Arc::new(LaneLifetime::new());
        let sender = ConnectionSender {
            lane_id: conn_id,
            connection_core: Arc::clone(&self.connection_core),
            failures: Arc::new(failures_tx),
            lane_closed: closed_rx.clone(),
            lane_generation: lane_generation.clone(),
            response_book: response_reservation::RequestBook::for_lane(lane_generation.clone()),
        };

        let parity = local_settings.parity;
        let handle_local_settings = local_settings.clone();
        let handle_peer_settings = peer_settings.clone();
        let lane_grant_state = Arc::new(Mutex::new(LaneGrant::empty()));
        trace!(%conn_id, "make_connection_handle: inserting slot into conns");
        if let Some(observer) = &self.observer {
            observer.driver_event(vox_types::DriverEvent::LaneOpened { lane_id: conn_id });
        }
        self.conns.insert(
            conn_id,
            ConnectionSlot::Active(LaneState {
                id: conn_id,
                local_settings,
                peer_settings,
                conn_tx,
                closed_tx,
                generation: lane_generation,
                schema_recv_tracker: Arc::new(vox_types::SchemaRecvTracker::new()),
                lane_grant: Arc::clone(&lane_grant_state),
                peer_channel_ids: HashSet::new(),
                received_schema_count: 0,
                received_schema_bytes: 0,
                peer_calls: 0,
            }),
        );

        Ok(LaneHandle {
            sender,
            rx: conn_rx,
            failures_rx,
            control_tx: Some(self.control_tx.clone()),
            closed_rx,
            local_settings: handle_local_settings,
            peer_settings: handle_peer_settings,
            max_peer_channel_credit: self
                .resource_limits
                .map(|limits| limits.max_peer_channel_credit),
            parity,
            observer: self.observer.clone(),
            peer_identity: self.peer_identity.clone(),
            peer_evidence: self.peer_evidence.clone(),
            lane_grant: LaneGrant::empty(),
            lane_grant_state,
        })
    }

    /// Run the connection recv loop: read from the conduit, demux by lane ID,
    /// and route to the appropriate lane's driver. Also processes
    /// open/close requests from the ConnectionHandle.
    // r[impl connection.message]
    pub async fn run(&mut self) {
        let mut keepalive_runtime = self.make_keepalive_runtime();
        let mut keepalive_tick = keepalive_runtime.as_ref().map(|_| {
            let mut interval = vox_types::time::tokio::interval(Duration::from_millis(10));
            interval.set_missed_tick_behavior(vox_types::time::tokio::MissedTickBehavior::Delay);
            interval
        });
        let mut open_rx_closed = false;
        let mut close_rx_closed = false;
        let mut control_rx_closed = false;

        loop {
            enum RunEvent {
                Message(std::io::Result<Option<SelfRef<Message<'static>>>>),
                Open(Option<OpenRequest>),
                Close(Option<CloseRequest>),
                Control(Option<DropControlRequest>),
                Keepalive,
            }

            let event = {
                let msg = self.rx.recv_msg().fuse();
                // Dropping public handles closes these local request queues, but
                // that is not a protocol shutdown. Once a queue reports `None`,
                // stop polling it so a biased select cannot spin on the closed
                // queue and starve recv/control work.
                let open = async {
                    if open_rx_closed {
                        futures_util::future::pending().await
                    } else {
                        self.open_rx.recv().await
                    }
                }
                .fuse();
                let close = async {
                    if close_rx_closed {
                        futures_util::future::pending().await
                    } else {
                        self.close_rx.recv().await
                    }
                }
                .fuse();
                let control = async {
                    if control_rx_closed {
                        futures_util::future::pending().await
                    } else {
                        self.control_rx.recv().await
                    }
                }
                .fuse();
                let keepalive = async {
                    if let Some(interval) = keepalive_tick.as_mut() {
                        interval.tick().await;
                    } else {
                        futures_util::future::pending::<()>().await;
                    }
                }
                .fuse();
                futures_util::pin_mut!(msg, open, close, control, keepalive);
                futures_util::select_biased! {
                    req = control => RunEvent::Control(req),
                    msg = msg => RunEvent::Message(msg),
                    req = open => RunEvent::Open(req),
                    req = close => RunEvent::Close(req),
                    () = keepalive => RunEvent::Keepalive,
                }
            };

            match event {
                RunEvent::Message(msg) => {
                    vox_types::dlog!("[connection {:?}] recv_msg returned", self.role);
                    match msg {
                        Ok(Some(msg)) => {
                            // Capture the frame's descriptors before the next
                            // recv overwrites them; thread them with the msg.
                            #[cfg(unix)]
                            let fds = self.rx.take_frame_fds();
                            #[cfg(not(unix))]
                            self.rx.take_frame_fds();
                            let limits = self.resource_limits;
                            let core = Arc::clone(&self.connection_core);
                            let completed = {
                                #[cfg(unix)]
                                let processing =
                                    self.handle_message(msg, fds, &mut keepalive_runtime);
                                #[cfg(not(unix))]
                                let processing =
                                    self.handle_message(msg, (), &mut keepalive_runtime);
                                let processing = async {
                                    if let Some(limits) = limits {
                                        vox_types::time::tokio::timeout(
                                            limits.max_message_processing_time,
                                            processing,
                                        )
                                        .await
                                        .is_ok()
                                    } else {
                                        processing.await;
                                        true
                                    }
                                }
                                .fuse();
                                let stopped = core.wait_admission_stopped().fuse();
                                futures_util::pin_mut!(processing, stopped);
                                futures_util::select_biased! {
                                    () = stopped => false,
                                    completed = processing => completed,
                                }
                            };
                            // Cancelling a partially handled message is an
                            // uncertain outcome, so terminate instead of
                            // resuming this connection.
                            if !completed {
                                self.resource_exhausted = true;
                            }
                            if self.resource_exhausted {
                                self.close_all_connections(ConnectionCloseReason::Local);
                                break;
                            }
                        }
                        Ok(None) => {
                            vox_types::dlog!(
                                "[connection {:?}] recv loop: conduit returned EOF",
                                self.role
                            );
                            self.close_all_connections(ConnectionCloseReason::Remote);
                            break;
                        }
                        Err(error) => {
                            let close_reason = classify_connection_recv_error(&error);
                            self.observe_connection_recv_error(&error);
                            warn!(
                                role = ?self.role,
                                %error,
                                ?close_reason,
                                "connection receive failed; closing connections if recovery is unavailable"
                            );
                            vox_types::dlog!(
                                "[connection {:?}] recv loop: conduit recv error: {}",
                                self.role,
                                error
                            );
                            self.close_all_connections(close_reason);
                            break;
                        }
                    }
                }
                RunEvent::Open(Some(req)) => {
                    let core = Arc::clone(&self.connection_core);
                    if interrupt_on_admission_stop(&core, self.handle_open_request(req))
                        .await
                        .is_none()
                    {
                        self.resource_exhausted = true;
                    }
                }
                RunEvent::Close(Some(req)) => {
                    let core = Arc::clone(&self.connection_core);
                    if interrupt_on_admission_stop(&core, self.handle_close_request(req))
                        .await
                        .is_none()
                    {
                        self.resource_exhausted = true;
                    }
                }
                RunEvent::Control(Some(req)) => {
                    let terminal = matches!(
                        &req,
                        DropControlRequest::Shutdown { .. } | DropControlRequest::ResourceClose
                    );
                    let keep_running = if terminal {
                        self.handle_drop_control_request(req).await
                    } else {
                        let core = Arc::clone(&self.connection_core);
                        match interrupt_on_admission_stop(
                            &core,
                            self.handle_drop_control_request(req),
                        )
                        .await
                        {
                            Some(result) => result,
                            None => {
                                self.resource_exhausted = true;
                                false
                            }
                        }
                    };
                    if !keep_running {
                        self.close_all_connections(ConnectionCloseReason::Local);
                        break;
                    }
                }
                RunEvent::Keepalive => {
                    let core = Arc::clone(&self.connection_core);
                    let keep_running = interrupt_on_admission_stop(
                        &core,
                        self.handle_keepalive_tick(&mut keepalive_runtime),
                    )
                    .await;
                    match keep_running {
                        Some(true) => {}
                        Some(false) => {
                            self.close_all_connections(ConnectionCloseReason::Protocol);
                            break;
                        }
                        None => {
                            self.resource_exhausted = true;
                            self.close_all_connections(ConnectionCloseReason::Local);
                            break;
                        }
                    }
                }
                RunEvent::Open(None) => {
                    open_rx_closed = true;
                }
                RunEvent::Close(None) => {
                    close_rx_closed = true;
                }
                RunEvent::Control(None) => {
                    control_rx_closed = true;
                }
            }
            if self.resource_exhausted {
                self.close_all_connections(ConnectionCloseReason::Local);
                break;
            }
        }

        // Partial gate for every terminal path, including observed EOF and
        // protocol failures. A later integration must carry one absolute
        // shutdown deadline through admission, drain and transport close.
        if !self.connection_core.admission_is_stopped()
            && vox_types::time::tokio::timeout(
                Duration::from_secs(1),
                self.connection_core.stop_outbound_admission(),
            )
            .await
            .is_err()
        {
            self.connection_core.force_stop_admission();
            self.outbound_abort.abort();
        }

        if self.resource_exhausted {
            self.outbound_abort.abort();
            if let Ok(mut inner) = self.connection_core.inner.try_lock() {
                inner.conns.clear();
                let tx = inner.tx.take();
                drop(inner);
                if let Some(tx) = tx {
                    tx.abort();
                }
            } else {
                warn!("sender lock unavailable; transport teardown remains uncertain");
            }
        }
        // Drop all lane slots so per-lane drivers exit immediately.
        self.close_all_connections(ConnectionCloseReason::ConnectionShutdown);
        trace!("connection recv loop exited");
    }

    async fn handle_message(
        &mut self,
        msg: SelfRef<Message<'static>>,
        fds: vox_types::FrameFds,
        keepalive_runtime: &mut Option<KeepaliveRuntime>,
    ) {
        let msg_ref = msg.get();
        let conn_id = msg_ref.lane_id;
        if let Some(limits) = self.resource_limits
            && let Some(ConnectionSlot::Active(state)) = self.conns.get_mut(&conn_id)
        {
            if matches!(&msg_ref.payload,
                MessagePayload::RequestMessage(request) if matches!(&request.body, RequestBody::Call(_)))
            {
                if state.peer_calls >= limits.max_peer_calls_per_lane {
                    self.resource_exhausted = true;
                    return;
                }
                state.peer_calls += 1;
            }
            if let MessagePayload::ChannelMessage(channel) = &msg_ref.payload
                && let vox_types::ChannelBody::GrantCredit(grant) = &channel.body
                && grant.additional > limits.max_peer_channel_credit
            {
                self.resource_exhausted = true;
                return;
            }
            let ids: &[vox_types::ChannelId] = match &msg_ref.payload {
                MessagePayload::RequestMessage(request) => match &request.body {
                    RequestBody::Call(call) => &call.channels,
                    _ => &[],
                },
                MessagePayload::ChannelMessage(channel) => std::slice::from_ref(&channel.id),
                _ => &[],
            };
            for id in ids {
                if !state.peer_channel_ids.contains(id) {
                    if state.peer_channel_ids.len() >= limits.max_peer_channel_ids_per_lane {
                        self.resource_exhausted = true;
                        return;
                    }
                    state.peer_channel_ids.insert(*id);
                }
            }
            let schema_len = match &msg_ref.payload {
                MessagePayload::SchemaMessage(schema) => Some(schema.schemas.0.len()),
                MessagePayload::RequestMessage(request) => match &request.body {
                    RequestBody::Call(call) if !call.schemas.is_empty() => {
                        Some(call.schemas.0.len())
                    }
                    RequestBody::Response(response) if !response.schemas.is_empty() => {
                        Some(response.schemas.0.len())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(bytes) = schema_len {
                if state.received_schema_count >= limits.max_schemas_per_lane
                    || bytes
                        > limits
                            .max_schema_bytes_per_lane
                            .saturating_sub(state.received_schema_bytes)
                {
                    self.resource_exhausted = true;
                    return;
                }
                state.received_schema_count += 1;
                state.received_schema_bytes += bytes;
            }
        }
        match &msg_ref.payload {
            MessagePayload::Ping(ping) => {
                if self.resource_limits.is_some() && !conn_id.is_control() {
                    self.resource_exhausted = true;
                    return;
                }
                // r[impl connection.keepalive]
                let _ = self
                    .connection_core
                    .send(
                        Message {
                            lane_id: conn_id,
                            payload: MessagePayload::Pong(vox_types::Pong { nonce: ping.nonce }),
                        },
                        None,
                        None,
                    )
                    .await;
                return;
            }
            MessagePayload::Pong(pong) => {
                if conn_id.is_control() {
                    // r[impl connection.keepalive]
                    self.handle_keepalive_pong(pong.nonce, keepalive_runtime);
                }
                return;
            }
            MessagePayload::SchemaMessage(schema_msg) => {
                let (schema_recv_tracker, conn_tx) = match self.conns.get(&conn_id) {
                    Some(ConnectionSlot::Active(state)) => (
                        Arc::clone(&state.schema_recv_tracker),
                        state.conn_tx.clone(),
                    ),
                    _ => return,
                };
                let _ = self.record_received_schema_bytes(
                    conn_id,
                    Arc::clone(&schema_recv_tracker),
                    schema_msg.method_id,
                    schema_msg.direction,
                    &schema_msg.schemas,
                    "standalone schema message",
                );
                let recv_msg = RecvMessage {
                    schemas: schema_recv_tracker,
                    msg: msg.map(|m| match m.payload {
                        MessagePayload::SchemaMessage(schema) => ConnectionMessage::Schema(schema),
                        _ => unreachable!(),
                    }),
                    fds,
                };
                if conn_tx.send(recv_msg).await.is_err() {
                    self.remove_connection_with_reason(&conn_id, ConnectionCloseReason::Unknown);
                }
                return;
            }
            _ => {}
        }
        vox_types::selfref_match!(msg, payload {
            // r[impl lane.close.semantics]
            MessagePayload::LaneClose(_) => {
                if conn_id.is_control() {
                    warn!("received LaneClose for control lane");
                } else {
                    trace!(conn_id = conn_id.0, "received LaneClose for service lane");
                }
                // Remove the service lane — dropping conn_tx causes the Driver's rx
                // to return None, which exits its run loop. All in-flight handlers
                // are dropped, triggering DriverReplySink::drop → Cancelled responses.
                self.remove_connection_with_reason(&conn_id, ConnectionCloseReason::Remote);
            }
            MessagePayload::LaneOpen(open) => {
                self.handle_inbound_open(conn_id, open).await;
            }
            MessagePayload::LaneAccept(accept) => {
                self.handle_inbound_accept(conn_id, accept);
            }
            MessagePayload::LaneReject(reject) => {
                self.handle_inbound_reject(conn_id, reject);
            }
            MessagePayload::RequestMessage(r) => {
                if !matches!(self.conns.get(&conn_id), Some(ConnectionSlot::Active(_))) {
                    return;
                }
                let r_ref = r.get();
                vox_types::dlog!(
                    "[connection {:?}] recv request: conn={:?} req={:?} body={} method={:?}",
                    self.role,
                    conn_id,
                    r_ref.id,
                    match &r_ref.body {
                        RequestBody::Call(_) => "Call",
                        RequestBody::Response(_) => "Response",
                        RequestBody::Cancel(_) => "Cancel",
                    },
                    match &r_ref.body {
                        RequestBody::Call(call) => Some(call.method_id),
                        RequestBody::Response(_) | RequestBody::Cancel(_) => None,
                    }
                );
                // Do not let a duplicate call replace the method binding used
                // by an earlier live request, including before its consumer
                // has had a chance to observe it. This is only the pre-write
                // guard; response-in-flight identity remains a separate gate.
                if let RequestBody::Call(call) = &r_ref.body
                    && !self.connection_core.record_incoming_call(
                        conn_id,
                        r_ref.id,
                        call.method_id,
                    )
                {
                    if send_drop_control(
                        &self.control_tx,
                        DropControlRequest::ProtocolClose {
                            conn_id,
                            description: format!("duplicate live request {:?}", r_ref.id),
                        },
                    )
                    .is_err()
                    {
                        self.resource_exhausted = true;
                    }
                    return;
                }
                // Record any inlined schemas from the incoming request before routing
                let response_had_schema_payload = matches!(&r_ref.body, RequestBody::Response(resp) if !resp.schemas.is_empty());
                {
                    let schema_bytes = match &r_ref.body {
                        RequestBody::Call(call) => Some(&call.schemas),
                        RequestBody::Response(resp) => Some(&resp.schemas),
                        _ => None,
                    };
                    vox_types::dlog!(
                        "[schema] recv ({:?}): req={:?} body={} schemas_len={:?}",
                        self.role,
                        r_ref.id,
                    match &r_ref.body {
                            RequestBody::Call(_) => "Call",
                            RequestBody::Response(_) => "Response",
                            RequestBody::Cancel(_) => "Cancel",
                        },
                        schema_bytes.map(|s| s.0.len())
                    );
                    let schema_recv_tracker = match self.conns.get(&conn_id) {
                        Some(ConnectionSlot::Active(state)) => {
                            Arc::clone(&state.schema_recv_tracker)
                        }
                        _ => return,
                    };
                    if let Some(schema_bytes) = schema_bytes
                        && !schema_bytes.is_empty()
                    {
                        let (method_id, direction) = match &r_ref.body {
                            RequestBody::Call(call) => {
                                (call.method_id, vox_types::BindingDirection::Args)
                            }
                            RequestBody::Response(_) => {
                                let Some(method_id) =
                                    self.connection_core.take_outgoing_call_method(conn_id, r_ref.id)
                                else {
                                    self.close_connection_for_protocol_error(
                                        conn_id,
                                        format!(
                                            "response schemas for unknown inflight request {:?}",
                                            r_ref.id
                                        ),
                                    );
                                    return;
                                };
                                (method_id, vox_types::BindingDirection::Response)
                            }
                            RequestBody::Cancel(_) => unreachable!(),
                        };
                        if !self.record_received_schema_bytes(
                            conn_id,
                            schema_recv_tracker,
                            method_id,
                            direction,
                            schema_bytes,
                            "inlined request schemas",
                        ) {
                            return;
                        }
                    }
                }
                if matches!(&r_ref.body, RequestBody::Response(_)) && !response_had_schema_payload {
                    let _ = self.connection_core.take_outgoing_call_method(conn_id, r_ref.id);
                }
                let state = match self.conns.get(&conn_id) {
                    Some(ConnectionSlot::Active(state)) => state,
                    _ => return,
                };
                let conn_tx = state.conn_tx.clone();
                let request_id = r_ref.id;
                let body_kind = match &r_ref.body {
                    RequestBody::Call(_) => "Call",
                    RequestBody::Response(_) => "Response",
                    RequestBody::Cancel(_) => "Cancel",
                };
                let recv_msg = RecvMessage {
                    schemas: Arc::clone(&state.schema_recv_tracker),
                    msg: r.map(ConnectionMessage::Request),
                    fds,
                };
                vox_types::dlog!(
                    "[connection {:?}] dispatch request: conn={:?} req={:?} body={}",
                    self.role,
                    conn_id,
                    request_id,
                    body_kind
                );
                if conn_tx.send(recv_msg).await.is_err() {
                    self.remove_connection_with_reason(&conn_id, ConnectionCloseReason::Unknown);
                }
            }
            MessagePayload::ChannelMessage(c) => {
                let state = match self.conns.get(&conn_id) {
                    Some(ConnectionSlot::Active(state)) => state,
                    _ => return,
                };
                let conn_tx = state.conn_tx.clone();
                let recv_msg = RecvMessage {
                    schemas: Arc::clone(&state.schema_recv_tracker),
                    msg: c.map(ConnectionMessage::Channel),
                    fds,
                };
                if conn_tx.send(recv_msg).await.is_err() {
                    self.remove_connection_with_reason(&conn_id, ConnectionCloseReason::Unknown);
                }
            }
            MessagePayload::ProtocolError(_) => {
                warn!(%conn_id, "received protocol error from peer");
                self.close_all_connections(ConnectionCloseReason::Protocol);
                let _ = send_drop_control(&self.control_tx, DropControlRequest::shutdown());
            }
        })
    }

    // r[impl connection.keepalive]
    fn make_keepalive_runtime(&self) -> Option<KeepaliveRuntime> {
        let config = self.keepalive?;
        if config.ping_interval.is_zero() || config.pong_timeout.is_zero() {
            warn!("keepalive disabled due to non-positive interval/timeout");
            return None;
        }
        let now = vox_types::time::tokio::Instant::now();
        Some(KeepaliveRuntime {
            ping_interval: config.ping_interval,
            pong_timeout: config.pong_timeout,
            next_ping_at: now + config.ping_interval,
            waiting_pong_nonce: None,
            pong_deadline: now,
            next_ping_nonce: 1,
        })
    }

    // r[impl connection.keepalive]
    fn handle_keepalive_pong(&self, nonce: u64, keepalive_runtime: &mut Option<KeepaliveRuntime>) {
        let Some(runtime) = keepalive_runtime.as_mut() else {
            return;
        };
        if runtime.waiting_pong_nonce != Some(nonce) {
            return;
        }
        runtime.waiting_pong_nonce = None;
        runtime.next_ping_at = vox_types::time::tokio::Instant::now() + runtime.ping_interval;
    }

    // r[impl connection.keepalive]
    async fn handle_keepalive_tick(
        &mut self,
        keepalive_runtime: &mut Option<KeepaliveRuntime>,
    ) -> bool {
        let Some(runtime) = keepalive_runtime.as_mut() else {
            return true;
        };
        let now = vox_types::time::tokio::Instant::now();

        if let Some(waiting_nonce) = runtime.waiting_pong_nonce {
            if now >= runtime.pong_deadline {
                warn!(
                    nonce = waiting_nonce,
                    timeout_ms = runtime.pong_timeout.as_millis(),
                    "keepalive timeout waiting for pong"
                );
                return false;
            }
            return true;
        }

        if now < runtime.next_ping_at {
            return true;
        }

        let nonce = runtime.next_ping_nonce;
        if self
            .connection_core
            .send(
                Message {
                    lane_id: LaneId::CONTROL,
                    payload: MessagePayload::Ping(vox_types::Ping { nonce }),
                },
                None,
                None,
            )
            .await
            .is_err()
        {
            warn!("failed to send keepalive ping");
            return false;
        }

        runtime.waiting_pong_nonce = Some(nonce);
        runtime.pong_deadline = now + runtime.pong_timeout;
        runtime.next_ping_at = now + runtime.ping_interval;
        runtime.next_ping_nonce = runtime.next_ping_nonce.wrapping_add(1);
        true
    }

    // r[impl lane.open.result]
    async fn send_lane_reject(
        connection_core: Arc<ConnectionCore>,
        conn_id: LaneId,
        rejection: LaneRejection,
    ) {
        let _ = connection_core
            .send(
                Message {
                    lane_id: conn_id,
                    payload: MessagePayload::LaneReject(vox_types::LaneReject {
                        metadata: rejection.into_metadata(),
                    }),
                },
                None,
                None,
            )
            .await;
    }

    // A peer that stops reading must not keep the connection alive forever
    // after a local limit is hit. The send is attempted before closing.
    async fn send_resource_reject(
        &mut self,
        conn_id: LaneId,
        reason: LaneRejectReason,
        message: &'static str,
    ) {
        let result = vox_types::time::tokio::timeout(
            Duration::from_secs(1),
            Self::send_lane_reject(
                Arc::clone(&self.connection_core),
                conn_id,
                LaneRejection::with_message(reason, message),
            ),
        )
        .await;
        if result.is_err() {
            self.resource_exhausted = true;
        }
    }

    // r[impl lane.open]
    // r[impl lane.wire]
    // r[impl lane.open.result]
    // r[impl lane.open.wire.rejection]
    // r[impl lane.open.settings]
    async fn handle_inbound_open(&mut self, conn_id: LaneId, open: SelfRef<LaneOpen>) {
        if let Some(limits) = self.resource_limits {
            if self.peer_lane_opens >= limits.max_peer_lane_opens {
                self.resource_exhausted = true;
                self.send_resource_reject(
                    conn_id,
                    LaneRejectReason::NotReady,
                    "lane open budget exhausted",
                )
                .await;
                return;
            }
            self.peer_lane_opens += 1;
            let service_lanes =
                self.conns.len() - usize::from(self.conns.contains_key(&LaneId::CONTROL));
            if service_lanes >= limits.max_service_lanes {
                self.send_resource_reject(
                    conn_id,
                    LaneRejectReason::NotReady,
                    "service lane limit reached",
                )
                .await;
                return;
            }
            let settings = &open.get().connection_settings;
            if settings.initial_channel_credit > limits.max_peer_channel_credit
                || settings.max_concurrent_requests > limits.max_peer_concurrent_requests
            {
                self.send_resource_reject(
                    conn_id,
                    LaneRejectReason::PolicyRejected,
                    "peer lane settings exceed resource limits",
                )
                .await;
                return;
            }
        }
        let establishment_started_at = observe_establishment_started(
            self.observer.as_ref(),
            self.role,
            EstablishmentPhase::ServiceLaneOpen,
            Some(conn_id),
        );

        // Validate: connection ID must match peer's parity (opposite of ours).
        let peer_parity = self.parity.other();
        if !conn_id.has_parity(peer_parity) {
            let rejection = LaneRejection::with_message(
                LaneRejectReason::PolicyRejected,
                "lane id parity does not match peer",
            );
            let details = lane_rejection_details(&rejection);
            Self::send_lane_reject(Arc::clone(&self.connection_core), conn_id, rejection).await;
            observe_establishment_finished_with_details(
                self.observer.as_ref(),
                self.role,
                EstablishmentPhase::ServiceLaneOpen,
                Some(conn_id),
                EstablishmentOutcome::Error,
                establishment_started_at,
                details,
            );
            return;
        }

        // Validate: connection ID must not already be in use.
        if self.conns.contains_key(&conn_id) {
            let rejection = LaneRejection::with_message(
                LaneRejectReason::PolicyRejected,
                "lane id is already in use",
            );
            let details = lane_rejection_details(&rejection);
            Self::send_lane_reject(Arc::clone(&self.connection_core), conn_id, rejection).await;
            observe_establishment_finished_with_details(
                self.observer.as_ref(),
                self.role,
                EstablishmentPhase::ServiceLaneOpen,
                Some(conn_id),
                EstablishmentOutcome::Error,
                establishment_started_at,
                details,
            );
            return;
        }

        // r[impl lane.open.wire.rejection]
        // Call the acceptor callback. If none is registered, reject.
        if self.lane_acceptor.is_none() {
            let authorization_started_at = observe_establishment_started(
                self.observer.as_ref(),
                self.role,
                EstablishmentPhase::LaneAuthorization,
                Some(conn_id),
            );
            let rejection = LaneRejection::with_message(
                LaneRejectReason::NotReady,
                "no lane acceptor configured",
            );
            let details = lane_rejection_details(&rejection);
            Self::send_lane_reject(Arc::clone(&self.connection_core), conn_id, rejection).await;
            observe_establishment_finished_with_details(
                self.observer.as_ref(),
                self.role,
                EstablishmentPhase::LaneAuthorization,
                Some(conn_id),
                EstablishmentOutcome::Rejected,
                authorization_started_at,
                details,
            );
            observe_establishment_finished_with_details(
                self.observer.as_ref(),
                self.role,
                EstablishmentPhase::ServiceLaneOpen,
                Some(conn_id),
                EstablishmentOutcome::Rejected,
                establishment_started_at,
                details,
            );
            return;
        }

        // Derive settings: opposite parity, same limits for now.
        let open = open.get();
        if open.connection_settings.initial_channel_credit == 0 {
            let rejection = LaneRejection::with_message(
                LaneRejectReason::PolicyRejected,
                "initial_channel_credit must be greater than zero",
            );
            let details = lane_rejection_details(&rejection);
            Self::send_lane_reject(Arc::clone(&self.connection_core), conn_id, rejection).await;
            observe_establishment_finished_with_details(
                self.observer.as_ref(),
                self.role,
                EstablishmentPhase::ServiceLaneOpen,
                Some(conn_id),
                EstablishmentOutcome::Error,
                establishment_started_at,
                details,
            );
            return;
        }

        let our_settings = ConnectionSettings {
            parity: open.connection_settings.parity.other(),
            max_concurrent_requests: self.resource_limits.map_or(
                open.connection_settings.max_concurrent_requests,
                |limits| {
                    self.local_connection_settings
                        .max_concurrent_requests
                        .min(limits.max_inbound_requests_per_lane)
                },
            ),
            initial_channel_credit: self.resource_limits.map_or(
                open.connection_settings.initial_channel_credit,
                |limits| {
                    self.local_connection_settings
                        .initial_channel_credit
                        .min(limits.max_initial_channel_credit)
                },
            ),
        };

        // Create the service lane handle and activate it.
        let handle = match self.try_make_connection_handle(
            conn_id,
            our_settings.clone(),
            open.connection_settings.clone(),
        ) {
            Ok(handle) => handle,
            Err(error) => {
                warn!(%conn_id, %error, "cannot establish service lane");
                self.resource_exhausted = true;
                self.connection_core.force_stop_admission();
                self.outbound_abort.abort();
                return;
            }
        };

        // Let the acceptor decide the service lane's fate.
        let metadata = open.metadata.clone();
        let request = match LaneRequest::new(&metadata, &self.peer_identity, &self.peer_evidence) {
            Ok(r) => r,
            Err(e) => {
                trace!(%conn_id, %e, "rejecting service lane");
                let authorization_started_at = observe_establishment_started(
                    self.observer.as_ref(),
                    self.role,
                    EstablishmentPhase::LaneAuthorization,
                    Some(conn_id),
                );
                let _ = self.remove_connection_with_reason(&conn_id, ConnectionCloseReason::Local);
                let rejection =
                    LaneRejection::with_message(LaneRejectReason::UnknownService, e.to_string());
                let details = lane_rejection_details(&rejection);
                Self::send_lane_reject(Arc::clone(&self.connection_core), conn_id, rejection).await;
                observe_establishment_finished_with_details(
                    self.observer.as_ref(),
                    self.role,
                    EstablishmentPhase::LaneAuthorization,
                    Some(conn_id),
                    EstablishmentOutcome::Rejected,
                    authorization_started_at,
                    details,
                );
                observe_establishment_finished_with_details(
                    self.observer.as_ref(),
                    self.role,
                    EstablishmentPhase::ServiceLaneOpen,
                    Some(conn_id),
                    EstablishmentOutcome::Rejected,
                    establishment_started_at,
                    details,
                );
                return;
            }
        };
        let pending = PendingLane::new(handle);
        let acceptor = self.lane_acceptor.as_ref().unwrap();
        trace!(%conn_id, "calling acceptor for service lane");
        let authorization_started_at = observe_establishment_started(
            self.observer.as_ref(),
            self.role,
            EstablishmentPhase::LaneAuthorization,
            Some(conn_id),
        );
        match acceptor.accept(&request, pending) {
            Ok(()) => {
                observe_establishment_finished(
                    self.observer.as_ref(),
                    self.role,
                    EstablishmentPhase::LaneAuthorization,
                    Some(conn_id),
                    EstablishmentOutcome::Ok,
                    authorization_started_at,
                );
                trace!(%conn_id, "acceptor accepted service lane, sending LaneAccept");
                let grant_started_at = observe_establishment_started(
                    self.observer.as_ref(),
                    self.role,
                    EstablishmentPhase::LaneGrant,
                    Some(conn_id),
                );
                let _ = self
                    .connection_core
                    .send(
                        Message {
                            lane_id: conn_id,
                            payload: MessagePayload::LaneAccept(vox_types::LaneAccept {
                                connection_settings: our_settings,
                                metadata: self.lane_grant_metadata(&conn_id),
                            }),
                        },
                        None,
                        None,
                    )
                    .await;
                observe_establishment_finished(
                    self.observer.as_ref(),
                    self.role,
                    EstablishmentPhase::LaneGrant,
                    Some(conn_id),
                    EstablishmentOutcome::Ok,
                    grant_started_at,
                );
                observe_establishment_finished(
                    self.observer.as_ref(),
                    self.role,
                    EstablishmentPhase::ServiceLaneOpen,
                    Some(conn_id),
                    EstablishmentOutcome::Ok,
                    establishment_started_at,
                );
            }
            Err(rejection) => {
                let details = lane_rejection_details(&rejection);
                observe_establishment_finished_with_details(
                    self.observer.as_ref(),
                    self.role,
                    EstablishmentPhase::LaneAuthorization,
                    Some(conn_id),
                    EstablishmentOutcome::Rejected,
                    authorization_started_at,
                    details,
                );
                // Clean up the connection slot we created.
                trace!(%conn_id, "acceptor rejected, removing conn slot");
                let _ = self.remove_connection_with_reason(&conn_id, ConnectionCloseReason::Local);
                Self::send_lane_reject(Arc::clone(&self.connection_core), conn_id, rejection).await;
                observe_establishment_finished_with_details(
                    self.observer.as_ref(),
                    self.role,
                    EstablishmentPhase::ServiceLaneOpen,
                    Some(conn_id),
                    EstablishmentOutcome::Rejected,
                    establishment_started_at,
                    details,
                );
            }
        }
    }

    // r[impl lane.open]
    // r[impl lane.wire]
    fn handle_inbound_accept(&mut self, conn_id: LaneId, accept: SelfRef<LaneAccept>) {
        let accept = accept.get();
        // A stray accept must never remove an active or retiring generation.
        if !matches!(
            self.conns.get(&conn_id),
            Some(ConnectionSlot::PendingOutbound(_))
        ) {
            return;
        }
        let slot = self.conns.remove(&conn_id);
        match slot {
            Some(ConnectionSlot::PendingOutbound(mut pending))
                if self.resource_limits.is_some_and(|limits| {
                    accept.connection_settings.initial_channel_credit
                        > limits.max_peer_channel_credit
                        || accept.connection_settings.max_concurrent_requests
                            > limits.max_peer_concurrent_requests
                }) =>
            {
                if let Some(tx) = pending.result_tx.take() {
                    let _ = tx.send(Err(ConnectionError::Protocol(
                        "peer lane settings exceed resource limits".into(),
                    )));
                }
            }
            Some(ConnectionSlot::PendingOutbound(mut pending))
                if accept.connection_settings.initial_channel_credit == 0 =>
            {
                observe_establishment_finished(
                    self.observer.as_ref(),
                    self.role,
                    EstablishmentPhase::ServiceLaneOpen,
                    Some(conn_id),
                    EstablishmentOutcome::Error,
                    pending.establishment_started_at,
                );
                if let Some(tx) = pending.result_tx.take() {
                    let _ = tx.send(Err(ConnectionError::Protocol(
                        "initial_channel_credit must be greater than zero".into(),
                    )));
                }
            }
            Some(ConnectionSlot::PendingOutbound(mut pending)) => {
                let mut handle = match self.try_make_connection_handle(
                    conn_id,
                    pending.local_settings.clone(),
                    accept.connection_settings.clone(),
                ) {
                    Ok(handle) => handle,
                    Err(error) => {
                        if let Some(tx) = pending.result_tx.take() {
                            let _ = tx.send(Err(error));
                        }
                        self.resource_exhausted = true;
                        self.connection_core.force_stop_admission();
                        self.outbound_abort.abort();
                        return;
                    }
                };
                let grant = LaneGrant::from_metadata(accept.metadata.clone());
                self.observe_lane_grant_creation(conn_id, &grant);
                handle.set_lane_grant(grant);

                observe_establishment_finished(
                    self.observer.as_ref(),
                    self.role,
                    EstablishmentPhase::ServiceLaneOpen,
                    Some(conn_id),
                    EstablishmentOutcome::Ok,
                    pending.establishment_started_at,
                );
                if let Some(tx) = pending.result_tx.take() {
                    let _ = tx.send(Ok(handle));
                }
            }
            Some(other) => {
                // Not pending outbound — put it back and ignore.
                self.conns.insert(conn_id, other);
            }
            None => {
                // No pending open for this ID — ignore.
            }
        }
    }

    // r[impl lane.open]
    // r[impl lane.wire]
    // r[impl lane.open.result]
    fn handle_inbound_reject(&mut self, conn_id: LaneId, reject: SelfRef<LaneReject>) {
        let reject = reject.get();
        // A stray reject must never remove an active or retiring generation.
        if !matches!(
            self.conns.get(&conn_id),
            Some(ConnectionSlot::PendingOutbound(_))
        ) {
            return;
        }
        let slot = self.conns.remove(&conn_id);
        match slot {
            Some(ConnectionSlot::PendingOutbound(mut pending)) => {
                let rejection = LaneRejection::from_metadata(reject.metadata.clone());
                let details = lane_rejection_details(&rejection);
                observe_establishment_finished_with_details(
                    self.observer.as_ref(),
                    self.role,
                    EstablishmentPhase::ServiceLaneOpen,
                    Some(conn_id),
                    EstablishmentOutcome::Rejected,
                    pending.establishment_started_at,
                    details,
                );
                if let Some(tx) = pending.result_tx.take() {
                    let _ = tx.send(Err(ConnectionError::Rejected(rejection)));
                }
            }
            Some(other) => {
                self.conns.insert(conn_id, other);
            }
            None => {}
        }
    }

    // r[impl lane.open.wire]
    // r[impl lane.open]
    // r[impl lane.wire]
    async fn handle_open_request(&mut self, req: OpenRequest) {
        if let Some(limits) = self.resource_limits {
            let service_lanes =
                self.conns.len() - usize::from(self.conns.contains_key(&LaneId::CONTROL));
            if service_lanes >= limits.max_service_lanes
                || req.settings.initial_channel_credit > limits.max_initial_channel_credit
                || req.settings.max_concurrent_requests > limits.max_inbound_requests_per_lane
            {
                let _ = req.result_tx.send(Err(ConnectionError::Protocol(
                    "local lane settings exceed resource limits".into(),
                )));
                return;
            }
        }
        if req.settings.initial_channel_credit == 0 {
            let _ = req.result_tx.send(Err(ConnectionError::Protocol(
                "initial_channel_credit must be greater than zero".into(),
            )));
            return;
        }

        let conn_id = self.conn_ids.alloc();
        if self.conns.contains_key(&conn_id) {
            let _ = req.result_tx.send(Err(ConnectionError::Protocol(
                "allocated lane id is still in use or has an unsettled retirement".into(),
            )));
            self.resource_exhausted = true;
            self.connection_core.force_stop_admission();
            self.outbound_abort.abort();
            return;
        }
        let establishment_started_at = observe_establishment_started(
            self.observer.as_ref(),
            self.role,
            EstablishmentPhase::ServiceLaneOpen,
            Some(conn_id),
        );

        // Send LaneOpen to the peer.
        let send_result = self
            .connection_core
            .send(
                Message {
                    lane_id: conn_id,
                    payload: MessagePayload::LaneOpen(LaneOpen {
                        connection_settings: req.settings.clone(),
                        metadata: req.metadata,
                    }),
                },
                None,
                None,
            )
            .await;

        if send_result.is_err() {
            observe_establishment_finished(
                self.observer.as_ref(),
                self.role,
                EstablishmentPhase::ServiceLaneOpen,
                Some(conn_id),
                EstablishmentOutcome::Error,
                establishment_started_at,
            );
            let _ = req.result_tx.send(Err(ConnectionError::Protocol(
                "failed to send LaneOpen".into(),
            )));
            return;
        }

        // Store the pending state. The run loop will complete the oneshot
        // when LaneAccept or LaneReject arrives.
        self.conns.insert(
            conn_id,
            ConnectionSlot::PendingOutbound(PendingOutboundData {
                local_settings: req.settings,
                establishment_started_at,
                result_tx: Some(req.result_tx),
            }),
        );
    }

    // r[impl lane.close]
    async fn handle_close_request(&mut self, req: CloseRequest) {
        if req.conn_id.is_control() {
            let _ = req.result_tx.send(Err(ConnectionError::Protocol(
                "cannot close control lane".into(),
            )));
            return;
        }

        // Remove the connection slot — this drops conn_tx and causes the
        // Driver to exit cleanly.
        if self
            .remove_connection_with_reason(&req.conn_id, ConnectionCloseReason::Local)
            .is_none()
        {
            let _ = req.result_tx.send(Err(ConnectionError::Protocol(
                "connection not found".into(),
            )));
            return;
        }

        if self.resource_exhausted {
            let _ = req.result_tx.send(Err(ConnectionError::Protocol(
                "lane retirement has an unsettled or unknown outcome".into(),
            )));
            return;
        }

        // Send LaneClose to the peer.
        let send_result = self
            .connection_core
            .send(
                Message {
                    lane_id: req.conn_id,
                    payload: MessagePayload::LaneClose(LaneClose {
                        metadata: req.metadata,
                    }),
                },
                None,
                None,
            )
            .await;

        if send_result.is_err() {
            let _ = req.result_tx.send(Err(ConnectionError::Protocol(
                "failed to send LaneClose".into(),
            )));
            return;
        }

        let _ = req.result_tx.send(Ok(()));
    }

    /// Attempt local drain and shared transport close under the original
    /// request deadline. Errors and cancellation request abort but do not
    /// establish that an independently owned writer has stopped.
    async fn shutdown_outbound_bounded(&mut self, deadline: Instant) -> std::io::Result<()> {
        let tx = self
            .connection_core
            .inner
            .try_lock()
            .map_err(|_| std::io::Error::other("connection sender lock unavailable"))?
            .tx
            .clone()
            .ok_or_else(|| std::io::Error::other("connection sender unavailable"))?;
        let mut guard = ShutdownAbortGuard {
            core: self.connection_core.clone(),
            tx: tx.clone(),
            worker: self.outbound_abort.clone(),
            armed: true,
        };
        self.connection_core
            .stop_and_fence(deadline)
            .await
            .map_err(|error| {
                let kind = match error {
                    FenceError::Deadline => std::io::ErrorKind::TimedOut,
                    FenceError::QueueClosed => std::io::ErrorKind::BrokenPipe,
                    FenceError::RejectedBatch => std::io::ErrorKind::Other,
                };
                std::io::Error::new(kind, format!("shutdown fence: {error:?}"))
            })?;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "shutdown deadline")
            })?;
        vox_types::time::tokio::timeout(remaining, tx.clone().close_shared_bounded(remaining))
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "shutdown deadline")
            })??;
        if deadline <= Instant::now() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "shutdown deadline",
            ));
        }
        let mut inner = self
            .connection_core
            .inner
            .try_lock()
            .map_err(|_| std::io::Error::other("connection sender lock unavailable"))?;
        if !inner
            .tx
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &tx))
        {
            return Err(std::io::Error::other("connection sender changed"));
        }
        inner.tx.take();
        drop(inner);
        if deadline <= Instant::now() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "shutdown deadline",
            ));
        }
        self.outbound_abort.abort();
        guard.armed = false;
        Ok(())
    }

    async fn handle_drop_control_request(&mut self, req: DropControlRequest) -> bool {
        match req {
            DropControlRequest::Shutdown { deadline } => {
                trace!("connection shutdown requested");
                if let Err(error) = self.shutdown_outbound_bounded(deadline).await {
                    warn!(%error, "local shutdown incomplete; requesting abort");
                    self.connection_core.force_stop_admission();
                    self.outbound_abort.abort();
                    self.resource_exhausted = true;
                }
                false
            }
            DropControlRequest::ResourceClose => {
                self.resource_exhausted = true;
                false
            }
            DropControlRequest::Close(conn_id) => {
                if conn_id.is_control() {
                    trace!("ignoring root close control request");
                    return true;
                }

                let removed = self
                    .remove_connection_with_reason(&conn_id, ConnectionCloseReason::Local)
                    .is_some();
                if self.resource_exhausted {
                    return false;
                }
                if removed {
                    let _ = self
                        .connection_core
                        .send(
                            Message {
                                lane_id: conn_id,
                                payload: MessagePayload::LaneClose(LaneClose {
                                    metadata: vox_types::Metadata::default(),
                                }),
                            },
                            None,
                            None,
                        )
                        .await;
                }

                true
            }
            DropControlRequest::ProtocolClose {
                conn_id,
                description,
            } => {
                trace!(%conn_id, %description, "protocol close requested");
                let send = self.connection_core.send(
                    Message {
                        lane_id: LaneId::CONTROL,
                        payload: MessagePayload::ProtocolError(vox_types::ProtocolError {
                            description: &description,
                        }),
                    },
                    None,
                    None,
                );
                if self.resource_limits.is_some() {
                    let _ = vox_types::time::tokio::timeout(Duration::from_secs(1), send).await;
                    self.resource_exhausted = true;
                } else {
                    let _ = send.await;
                }
                self.close_all_connections(ConnectionCloseReason::Protocol);
                false
            }
        }
    }

    fn lane_grant_metadata(&self, conn_id: &LaneId) -> Metadata {
        let Some(ConnectionSlot::Active(state)) = self.conns.get(conn_id) else {
            return Metadata::default();
        };
        state
            .lane_grant
            .lock()
            .expect("lane grant state mutex poisoned")
            .metadata()
            .clone()
    }

    fn observe_lane_grant_creation(&self, conn_id: LaneId, grant: &LaneGrant) {
        if grant.is_empty() {
            return;
        }
        let started_at = observe_establishment_started(
            self.observer.as_ref(),
            self.role,
            EstablishmentPhase::LaneGrant,
            Some(conn_id),
        );
        observe_establishment_finished(
            self.observer.as_ref(),
            self.role,
            EstablishmentPhase::LaneGrant,
            Some(conn_id),
            EstablishmentOutcome::Ok,
            started_at,
        );
    }

    fn observe_lane_grant_revocation(&self, conn_id: LaneId, state: &LaneState) {
        let has_grant = !state
            .lane_grant
            .lock()
            .expect("lane grant state mutex poisoned")
            .is_empty();
        if !has_grant {
            return;
        }
        let started_at = observe_establishment_started(
            self.observer.as_ref(),
            self.role,
            EstablishmentPhase::LaneGrantRevocation,
            Some(conn_id),
        );
        observe_establishment_finished(
            self.observer.as_ref(),
            self.role,
            EstablishmentPhase::LaneGrantRevocation,
            Some(conn_id),
            EstablishmentOutcome::Ok,
            started_at,
        );
    }

    fn remove_connection_with_reason(
        &mut self,
        conn_id: &LaneId,
        reason: ConnectionCloseReason,
    ) -> Option<ConnectionSlot> {
        trace!(%conn_id, "remove_connection called");
        let slot = self.conns.remove(conn_id);
        if let Some(ConnectionSlot::Retiring(generation)) = &slot {
            self.conns
                .insert(*conn_id, ConnectionSlot::Retiring(generation.clone()));
            return None;
        }
        if let Some(ConnectionSlot::Active(state)) = &slot {
            let retirement = state.generation.try_begin_retirement();
            if !matches!(retirement, Ok(lane_retirement::LanePhase::Retired)) {
                // Busy, poison, an active write, and quarantine are all
                // uncertain. Preserve the ID and stop this connection; abort
                // is only a request and does not prove transport teardown.
                self.conns
                    .insert(*conn_id, ConnectionSlot::Retiring(state.generation.clone()));
                self.resource_exhausted = true;
                self.connection_core.force_stop_admission();
                self.outbound_abort.abort();
            }
            let _ = state.closed_tx.send(Some(reason));
            self.connection_core
                .fail_lane_gates(*conn_id, &state.generation);
            self.observe_lane_grant_revocation(*conn_id, state);
            if let Some(observer) = &self.observer {
                observer.driver_event(vox_types::DriverEvent::LaneClosed {
                    lane_id: *conn_id,
                    reason,
                });
            }
        }
        slot
    }

    // r[impl rpc.observability.connection-errors]
    fn close_all_connections(&mut self, reason: ConnectionCloseReason) {
        trace!(role = ?self.role, count = self.conns.len(), "close_all_connections");
        vox_types::dlog!(
            "[connection {:?}] close_all_lanes: {} slots",
            self.role,
            self.conns.len()
        );
        for (conn_id, slot) in self.conns.iter() {
            if let ConnectionSlot::Active(state) = slot {
                vox_types::dlog!(
                    "[connection {:?}] closing connection {:?}",
                    self.role,
                    conn_id
                );
                // Never synchronously wait for the lane lock during shutdown.
                // A busy or poisoned result remains an unconfirmed teardown.
                let _ = state.generation.try_begin_retirement();
                let _ = state.closed_tx.send(Some(reason));
                self.connection_core
                    .fail_lane_gates(*conn_id, &state.generation);
                self.observe_lane_grant_revocation(*conn_id, state);
                if let Some(observer) = &self.observer {
                    observer.driver_event(vox_types::DriverEvent::LaneClosed {
                        lane_id: *conn_id,
                        reason,
                    });
                }
            }
        }
        self.conns.clear();
    }
}

/// A one-shot open gate for a locally-opened outbound channel. Created CLOSED when
/// the channel id is allocated during a Call's arg-encode, and opened once that Call
/// has been pushed to the outbound queue. Channel items wait on it, so a `tx.send`
/// the application fires concurrently with the call cannot reach the wire before the
/// Call that declares the channel — the sender upholds the frame-ordering invariant
/// (`r[impl rpc.channel.item]`).
pub(crate) struct ConnectionCore {
    inner: std::sync::Mutex<ConnectionCoreInner>,
    outbound_tx: mpsc::Sender<OutboundCommand>,
    admission: vox_rt::sync::Mutex<bool>,
    admission_stopped: std::sync::atomic::AtomicBool,
    guarded_responses: bool,
    admission_stopped_tx: watch::Sender<bool>,
    _admission_stopped_rx: watch::Receiver<bool>,
    admission_waiters: wake_registry::WakeRegistry,
    #[cfg(not(target_arch = "wasm32"))]
    owner_closed: watch::Receiver<()>,
    #[cfg(test)]
    before_admission_commit: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    before_worker_write: Arc<std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>>,
    #[cfg(test)]
    injected_admission_notification_failures: std::sync::atomic::AtomicUsize,
    observer: Option<VoxObserverHandle>,
    /// Open gates for channels the local side opened but whose declaring Call has not
    /// yet been enqueued. The generation distinguishes reused numeric lane IDs.
    channel_gates: std::sync::Mutex<
        HashMap<(LaneId, usize, vox_types::ChannelId), std::sync::Weak<ChannelDeclarationGate>>,
    >,
}

pub trait OutboundSendFuture: Future<Output = std::io::Result<()>> + MaybeSend + 'static {}
impl<T> OutboundSendFuture for T where T: Future<Output = std::io::Result<()>> + MaybeSend + 'static {}

type OutboundSend = Pin<Box<dyn OutboundSendFuture>>;

#[derive(Clone)]
struct PendingSchemaSend {
    method_id: vox_types::MethodId,
    direction: vox_types::BindingDirection,
    prepared: vox_types::PreparedSchemaPlan,
}

impl From<vox_types::ChannelWriterSchemaPlan> for PendingSchemaSend {
    fn from(plan: vox_types::ChannelWriterSchemaPlan) -> Self {
        let _ = plan.role;
        Self {
            method_id: plan.method_id,
            direction: plan.direction,
            prepared: plan.prepared,
        }
    }
}

// A fence is ordered behind all commands already accepted by this queue.
// Its acknowledgment reports only that the worker reached the fence: it does
// not establish peer receipt or settlement for an unqualified transport.
enum OutboundCommand {
    Batch(Box<OutboundBatch>),
    Fence(oneshot::Sender<Result<(), ()>>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FenceError {
    Deadline,
    QueueClosed,
    RejectedBatch,
}

struct OutboundBatch {
    conn_id: LaneId,
    lane_closed: Option<watch::Receiver<Option<ConnectionCloseReason>>>,
    lane_generation: Option<Arc<LaneLifetime>>,
    // Not yet populated by the production reply paths. Until every response
    // carries its ingress identity, legacy response sends remain unqualified.
    queued_response: Option<response_reservation::QueuedResponse>,
    #[cfg(test)]
    before_worker_write: Arc<std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>>,
    request_id: Option<RequestId>,
    outgoing_call: Option<(RequestId, vox_types::MethodId)>,
    incoming_response: Option<(RequestId, vox_types::MethodId)>,
    payload_kind: &'static str,
    conn_state: Arc<std::sync::Mutex<SendConnState>>,
    tx: Arc<dyn DynConduitTx>,
    schema_sends: Vec<PendingSchemaSend>,
    payload_send: OutboundSend,
    result_tx: oneshot::Sender<std::io::Result<()>>,
}

type PreparedOutboundBatch = (
    OutboundBatch,
    oneshot::Receiver<std::io::Result<()>>,
    Vec<vox_types::ChannelId>,
    DeclaredChannelGuard,
);

struct PrepareOutboundError {
    gated_channels: Vec<vox_types::ChannelId>,
    gate_guard: DeclaredChannelGuard,
}

/// Retains declaring gates through admission. Failure or cancellation before
/// queue commit makes each still-live channel terminal instead of opening it.
struct DeclaredChannelGuard {
    gates: Vec<Arc<ChannelDeclarationGate>>,
    committed: bool,
}

std::thread_local! {
    // Encoding is synchronous. The stack preserves nested encodes and lets an
    // unwinding scope fail gates even before the channel collector returns IDs.
    static DECLARATION_CAPTURES: std::cell::RefCell<Vec<DeclarationCaptureFrame>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct DeclarationCaptureFrame {
    core: usize,
    lane_id: LaneId,
    generation: Option<usize>,
    marker: std::rc::Rc<()>,
    active: std::rc::Rc<std::cell::Cell<bool>>,
    gates: std::rc::Rc<std::cell::RefCell<Vec<Arc<ChannelDeclarationGate>>>>,
}

struct DeclarationCapture<'a> {
    core: &'a ConnectionCore,
    marker: std::rc::Rc<()>,
    active: std::rc::Rc<std::cell::Cell<bool>>,
    gates: std::rc::Rc<std::cell::RefCell<Vec<Arc<ChannelDeclarationGate>>>>,
}

impl<'a> DeclarationCapture<'a> {
    fn stop_admission(core: &ConnectionCore) {
        core.force_stop_admission();
    }

    fn begin(
        core: &'a ConnectionCore,
        lane_id: LaneId,
        generation: Option<&Arc<LaneLifetime>>,
    ) -> Self {
        let marker = std::rc::Rc::new(());
        let active = std::rc::Rc::new(std::cell::Cell::new(true));
        let gates = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let frame = DeclarationCaptureFrame {
            core: std::ptr::from_ref(core) as usize,
            lane_id,
            generation: generation.map(|generation| Arc::as_ptr(generation) as usize),
            marker: marker.clone(),
            active: active.clone(),
            gates: gates.clone(),
        };
        let inserted = DECLARATION_CAPTURES
            .try_with(|captures| {
                let Ok(mut captures) = captures.try_borrow_mut() else {
                    return false;
                };
                captures.retain(|frame| frame.active.get());
                captures.push(frame);
                true
            })
            .unwrap_or(false);
        if !inserted {
            active.set(false);
            Self::stop_admission(core);
        }
        Self {
            core,
            marker,
            active,
            gates,
        }
    }

    fn record(
        core: &ConnectionCore,
        lane_id: LaneId,
        generation: &Arc<LaneLifetime>,
        gate: &Arc<ChannelDeclarationGate>,
    ) {
        let recorded = DECLARATION_CAPTURES
            .try_with(|captures| {
                let Ok(captures) = captures.try_borrow() else {
                    return false;
                };
                let Some(current) = captures.last() else {
                    return true;
                };
                if !current.active.get() {
                    return false;
                }
                if current.core != std::ptr::from_ref(core) as usize || current.lane_id != lane_id {
                    return true;
                }
                if current.generation != Some(Arc::as_ptr(generation) as usize) {
                    return true;
                }
                let Ok(mut gates) = current.gates.try_borrow_mut() else {
                    return false;
                };
                gates.push(Arc::clone(gate));
                true
            })
            .unwrap_or(false);
        if !recorded {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                gate.finish(GateState::Failed)
            }));
            Self::stop_admission(core);
        }
    }

    fn finish(mut self) -> DeclaredChannelGuard {
        let (gates, safe) = self.take();
        if !safe {
            for gate in &gates {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    gate.finish(GateState::Failed)
                }));
            }
        }
        DeclaredChannelGuard {
            gates,
            committed: false,
        }
    }

    fn take(&mut self) -> (Vec<Arc<ChannelDeclarationGate>>, bool) {
        let was_active = self.active.replace(false);
        let detached = if was_active {
            DECLARATION_CAPTURES
                .try_with(|captures| {
                    let Ok(mut captures) = captures.try_borrow_mut() else {
                        return false;
                    };
                    let Some(pos) = captures
                        .iter()
                        .position(|frame| std::rc::Rc::ptr_eq(&frame.marker, &self.marker))
                    else {
                        return false;
                    };
                    let was_last = pos + 1 == captures.len();
                    captures.remove(pos);
                    was_last
                })
                .unwrap_or(false)
        } else {
            false
        };
        let gates = self
            .gates
            .try_borrow_mut()
            .map(|mut gates| std::mem::take(&mut *gates));
        let safe = detached && gates.is_ok();
        if !safe {
            Self::stop_admission(self.core);
        }
        (gates.unwrap_or_default(), safe)
    }
}

impl Drop for DeclarationCapture<'_> {
    fn drop(&mut self) {
        if self.active.get() {
            let (gates, _) = self.take();
            drop(DeclaredChannelGuard {
                gates,
                committed: false,
            });
        }
    }
}

impl DeclaredChannelGuard {
    fn open(mut self) {
        for gate in &self.gates {
            gate.finish(GateState::Open);
        }
        self.committed = true;
    }
}

impl Drop for DeclaredChannelGuard {
    fn drop(&mut self) {
        if !self.committed {
            for gate in &self.gates {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    gate.finish(GateState::Failed)
                }));
            }
        }
    }
}

async fn run_outbound_worker(
    mut rx: mpsc::Receiver<OutboundCommand>,
    control_tx: mpsc::UnboundedSender<DropControlRequest>,
) {
    let mut rejected_batch = false;
    while let Some(command) = rx.recv().await {
        let batch = match command {
            OutboundCommand::Batch(batch) => *batch,
            OutboundCommand::Fence(reached) => {
                let _ = reached.send(if rejected_batch { Err(()) } else { Ok(()) });
                continue;
            }
        };
        if batch
            .lane_closed
            .as_ref()
            .is_some_and(|closed| closed.borrow().is_some())
        {
            rejected_batch = true;
            let _ = batch.result_tx.send(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "originating lane retired before write",
            )));
            continue;
        }
        #[cfg(test)]
        if let Some(hook) = batch.before_worker_write.lock().unwrap().take() {
            hook();
        }
        // Every lane write is serialized with retirement. A successful send
        // on an unqualified transport still leaves sticky uncertainty: its
        // background writer might continue after the send future returns.
        let locally_settled_transport = batch.tx.send_success_is_locally_settled();
        let write_lease = if let Some(generation) = &batch.lane_generation {
            match generation.acquire_write() {
                Ok(lease) => Some(lease),
                Err(error) => {
                    rejected_batch = true;
                    let fatal = !matches!(error, lane_retirement::LaneWriteError::NotOpen);
                    let _ = batch.result_tx.send(Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "originating lane cannot acquire a write lease",
                    )));
                    if fatal {
                        batch.tx.abort();
                        let _ = send_drop_control(&control_tx, DropControlRequest::ResourceClose);
                        break;
                    }
                    continue;
                }
            }
        } else {
            None
        };
        let mut queued_response = batch.queued_response;
        if let Some(response) = queued_response.as_ref() {
            let identity_matches = batch.payload_kind == "request.response"
                && batch.incoming_response.is_some_and(|(id, method)| {
                    batch.request_id == Some(id) && response.matches_request(id, method)
                });
            let valid = locally_settled_transport
                && identity_matches
                && write_lease
                    .as_ref()
                    .is_some_and(|lease| response.begin_write(lease).is_ok());
            if !valid {
                let _ = batch.result_tx.send(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "response has no valid lane-bound write lease",
                )));
                batch.tx.abort();
                let _ = send_drop_control(&control_tx, DropControlRequest::ResourceClose);
                break;
            }
        }
        // Only a batch accepted by the queue can become an in-flight call. Do
        // this before writing either its schema or its request to the transport,
        // so a response cannot arrive before its method binding is registered.
        if let Some((request_id, method_id)) = batch.outgoing_call {
            batch
                .conn_state
                .lock()
                .expect("send conn state mutex poisoned")
                .inflight_outgoing
                .insert(request_id, method_id);
        }
        if let Some((request_id, method_id)) = batch.incoming_response {
            let mut state = batch
                .conn_state
                .lock()
                .expect("send conn state mutex poisoned");
            if state.inflight_incoming.get(&request_id) == Some(&method_id) {
                state.inflight_incoming.remove(&request_id);
            }
        }
        trace!(
            conn_id = %batch.conn_id,
            request_id = ?batch.request_id,
            payload_kind = batch.payload_kind,
            schema_count = batch.schema_sends.len(),
            "connection outbound worker received batch"
        );
        let mut result = Ok(());
        for schema_send in batch.schema_sends {
            trace!(
                conn_id = %batch.conn_id,
                request_id = ?batch.request_id,
                method_id = ?schema_send.method_id,
                direction = ?schema_send.direction,
                "connection outbound worker sending schema batch"
            );
            let schemas = {
                let mut conn_state = batch
                    .conn_state
                    .lock()
                    .expect("send conn state mutex poisoned");
                conn_state.send_tracker.preview_prepared_plan(
                    schema_send.method_id,
                    schema_send.direction,
                    &schema_send.prepared,
                )
            };
            if schemas.is_empty() {
                continue;
            }

            let schema_msg = Message {
                lane_id: batch.conn_id,
                payload: MessagePayload::SchemaMessage(SchemaMessage {
                    method_id: schema_send.method_id,
                    direction: schema_send.direction,
                    schemas,
                }),
            };
            let send = match batch.tx.clone().prepare_msg(schema_msg, None) {
                Ok(send) => send,
                Err(error) => {
                    result = Err(error);
                    break;
                }
            };
            if let Err(error) = send.await {
                result = Err(error);
                break;
            }
            let mut conn_state = batch
                .conn_state
                .lock()
                .expect("send conn state mutex poisoned");
            conn_state.send_tracker.mark_prepared_plan_sent(
                schema_send.method_id,
                schema_send.direction,
                &schema_send.prepared,
            );
            conn_state
                .planned_bindings
                .remove(&(schema_send.direction, schema_send.method_id));
        }
        if result.is_ok()
            && let Err(error) = batch.payload_send.await
        {
            trace!(
                conn_id = %batch.conn_id,
                request_id = ?batch.request_id,
                payload_kind = batch.payload_kind,
                ?error,
                "connection outbound worker payload send failed"
            );
            result = Err(error);
        }
        trace!(
            conn_id = %batch.conn_id,
            request_id = ?batch.request_id,
            payload_kind = batch.payload_kind,
            ok = result.is_ok(),
            "connection outbound worker finished batch"
        );
        if let Some(response) = queued_response.take()
            && let Some(lease) = write_lease.as_ref()
        {
            let locally_successful = result.is_ok();
            if response.settle(lease, locally_successful).is_err() && locally_successful {
                result = Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "response identity settlement could not be established",
                ));
            }
        }
        if let Some(lease) = write_lease {
            let locally_successful = result.is_ok();
            let settlement = if locally_successful && !locally_settled_transport {
                lease.settle_unconfirmed()
            } else {
                lease.settle(locally_successful)
            };
            if locally_successful
                && !matches!(
                    settlement,
                    Ok(lane_retirement::LanePhase::Open | lane_retirement::LanePhase::Retired)
                )
            {
                result = Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "lane write settlement could not be established",
                ));
            }
        }
        let failed = result.is_err();
        if failed {
            // A write error may represent a partial frame. Never send later
            // queued batches on this worker after such an uncertain outcome.
            // `abort` is best effort and is not proof of transport teardown.
            batch.tx.abort();
            // Ask the connection driver to retire the lane state and attempt
            // teardown. A driver stalled in a handler may not observe this yet.
            let _ = send_drop_control(&control_tx, DropControlRequest::ResourceClose);
        }
        let _ = batch.result_tx.send(result);
        if failed {
            // Dropping the receiver fails queued results and capacity waiters.
            break;
        }
    }
}

/// Stop waiting on a local handler when a terminal request arrives. The
/// interrupted operation may already have written bytes; callers must treat
/// it as uncertain and terminate the connection, not retry it on this lane.
async fn interrupt_on_admission_stop<F: std::future::Future>(
    core: &ConnectionCore,
    operation: F,
) -> Option<F::Output> {
    let operation = operation.fuse();
    let stopped = core.wait_admission_stopped().fuse();
    futures_util::pin_mut!(operation, stopped);
    futures_util::select_biased! {
        () = stopped => None,
        output = operation => Some(output),
    }
}

/// Deliver control requests independently of potentially stalled message
/// handling. Terminal requests wake the admission signal before forwarding;
/// this is a shutdown request, not proof of transport closure.
async fn run_control_pump(
    mut source: mpsc::UnboundedReceiver<DropControlRequest>,
    destination: mpsc::UnboundedSender<DropControlRequest>,
    core: std::sync::Weak<ConnectionCore>,
) {
    while let Some(request) = source.recv().await {
        if matches!(
            &request,
            DropControlRequest::Shutdown { .. } | DropControlRequest::ResourceClose
        ) {
            if let Some(core) = core.upgrade() {
                core.force_stop_admission();
            } else {
                break;
            }
        }
        if destination.send(request).is_err() {
            break;
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_control_pump(
    source: mpsc::UnboundedReceiver<DropControlRequest>,
    destination: mpsc::UnboundedSender<DropControlRequest>,
    core: std::sync::Weak<ConnectionCore>,
) -> futures_util::future::AbortHandle {
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    let pump = async move {
        let _ = futures_util::future::Abortable::new(
            run_control_pump(source, destination, core),
            registration,
        )
        .await;
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(pump);
    } else {
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build control pump runtime");
            runtime.block_on(pump);
        });
    }
    abort
}

#[cfg(target_arch = "wasm32")]
fn spawn_control_pump(
    source: mpsc::UnboundedReceiver<DropControlRequest>,
    destination: mpsc::UnboundedSender<DropControlRequest>,
    core: std::sync::Weak<ConnectionCore>,
) -> futures_util::future::AbortHandle {
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    wasm_bindgen_futures::spawn_local(async move {
        let _ = futures_util::future::Abortable::new(
            run_control_pump(source, destination, core),
            registration,
        )
        .await;
    });
    abort
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_outbound_worker(
    rx: mpsc::Receiver<OutboundCommand>,
    control_tx: mpsc::UnboundedSender<DropControlRequest>,
) -> futures_util::future::AbortHandle {
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    let worker = async move {
        let _ =
            futures_util::future::Abortable::new(run_outbound_worker(rx, control_tx), registration)
                .await;
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(worker);
        return abort;
    }

    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build outbound worker runtime");
        runtime.block_on(worker);
    });
    abort
}

#[cfg(target_arch = "wasm32")]
fn spawn_outbound_worker(
    rx: mpsc::Receiver<OutboundCommand>,
    control_tx: mpsc::UnboundedSender<DropControlRequest>,
) -> futures_util::future::AbortHandle {
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    wasm_bindgen_futures::spawn_local(async move {
        let _ =
            futures_util::future::Abortable::new(run_outbound_worker(rx, control_tx), registration)
                .await;
    });
    abort
}

struct SendConnState {
    /// Tracks which schemas we have sent on this connection.
    send_tracker: vox_types::SchemaSendTracker,

    /// Maps request_id → method_id for in-flight incoming calls, so we can
    /// look up the method_id when sending the response.
    inflight_incoming: HashMap<RequestId, vox_types::MethodId>,

    /// Maps request_id → method_id for outbound calls awaiting a response, so
    /// inbound response schema payloads can bind their root TypeRef.
    inflight_outgoing: HashMap<RequestId, vox_types::MethodId>,

    /// Structured schema plans cached per binding until the first committed send.
    planned_bindings:
        HashMap<(vox_types::BindingDirection, vox_types::MethodId), vox_types::PreparedSchemaPlan>,
}

impl SendConnState {
    fn new() -> Self {
        SendConnState {
            send_tracker: vox_types::SchemaSendTracker::new(),
            inflight_incoming: HashMap::new(),
            inflight_outgoing: HashMap::new(),
            planned_bindings: HashMap::new(),
        }
    }
}

struct ConnectionCoreInner {
    /// Underlying conduit (tx end)
    tx: Option<Arc<dyn DynConduitTx>>,

    /// Per-connection state re: sent schemas, etc.
    conns: HashMap<LaneId, Arc<std::sync::Mutex<SendConnState>>>,
}

fn get_or_create_send_conn_state(
    inner: &mut ConnectionCoreInner,
    conn_id: LaneId,
) -> Arc<std::sync::Mutex<SendConnState>> {
    inner
        .conns
        .entry(conn_id)
        .or_insert_with(|| Arc::new(std::sync::Mutex::new(SendConnState::new())))
        .clone()
}

/// The channel id whose open-gate must be honored before sending `msg`, if `msg` is an
/// outbound channel item or close. Other messages (Calls, credit, reset, non-channel)
/// are never gated.
fn gated_channel_id(msg: &Message<'_>) -> Option<vox_types::ChannelId> {
    match &msg.payload {
        MessagePayload::ChannelMessage(ch) => match &ch.body {
            vox_types::ChannelBody::Item(_) | vox_types::ChannelBody::Close(_) => Some(ch.id),
            _ => None,
        },
        _ => None,
    }
}

impl ConnectionCore {
    fn fail_lane_gates(&self, lane_id: LaneId, generation: &Arc<LaneLifetime>) {
        let generation_id = Arc::as_ptr(generation) as usize;
        let pending = {
            let mut gates = match self.channel_gates.lock() {
                Ok(gates) => gates,
                Err(poisoned) => {
                    DeclarationCapture::stop_admission(self);
                    poisoned.into_inner()
                }
            };
            let keys: Vec<_> = gates
                .keys()
                .filter(|(lane, generation, _)| *lane == lane_id && *generation == generation_id)
                .copied()
                .collect();
            keys.into_iter()
                .filter_map(|key| gates.remove(&key).and_then(|gate| gate.upgrade()))
                .collect::<Vec<_>>()
        };
        for gate in pending {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                gate.finish(GateState::Failed)
            }));
        }
    }

    fn admission_is_stopped(&self) -> bool {
        self.admission_stopped
            .load(std::sync::atomic::Ordering::Acquire)
    }

    fn force_stop_admission(&self) {
        self.admission_stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.notify_admission_stopped();
        self.admission_waiters.wake_all();
    }

    fn notify_admission_stopped(&self) {
        #[cfg(test)]
        if self
            .injected_admission_notification_failures
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            return;
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.admission_stopped_tx.send(true)
        }));
    }

    async fn wait_admission_stopped(&self) {
        let waiter = self.admission_waiters.register();
        let stopped = futures_util::future::poll_fn(|cx| {
            waiter.register(cx.waker());
            if self.admission_is_stopped() {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        })
        .fuse();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut owner_closed = self.owner_closed.clone();
            let owner_closed = owner_closed.changed().fuse();
            futures_util::pin_mut!(stopped, owner_closed);
            futures_util::select_biased! {
                () = stopped => {},
                _ = owner_closed => {},
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            stopped.await;
        }
    }

    async fn wait_lane_closed(lane: &Option<watch::Receiver<Option<ConnectionCloseReason>>>) {
        let Some(mut lane) = lane.clone() else {
            std::future::pending::<()>().await;
            return;
        };
        if lane.borrow().is_some() {
            return;
        }
        while lane.changed().await.is_ok() {
            if lane.borrow().is_some() {
                return;
            }
        }
    }

    async fn stop_outbound_admission(&self) {
        let mut closed = self.admission.lock().await;
        *closed = true;
        self.force_stop_admission();
    }

    /// Establish the admission cutoff and wait for the worker to reach an
    /// ordered fence, applying one absolute deadline to the asynchronous
    /// stages. Synchronous wakeups are not preempted by that deadline. This
    /// is an experimental building block, not a complete transport close. On error
    /// or cancellation the owner must abort/quarantine uncertain work; a
    /// dropped fence or abort request is not proof of transport teardown.
    async fn stop_and_fence(&self, deadline: Instant) -> Result<(), FenceError> {
        self.force_stop_admission();
        let remaining = || {
            deadline
                .checked_duration_since(Instant::now())
                .filter(|duration| !duration.is_zero())
                .ok_or(FenceError::Deadline)
        };
        // Even when the wake signal is already set, take the mutex: a sender
        // that passed its final check may still be committing under this lock.
        vox_types::time::tokio::timeout(remaining()?, self.stop_outbound_admission())
            .await
            .map_err(|_| FenceError::Deadline)?;
        let (reached_tx, reached_rx) = oneshot::channel("connection.shutdown_fence");
        vox_types::time::tokio::timeout(
            remaining()?,
            self.outbound_tx.send(OutboundCommand::Fence(reached_tx)),
        )
        .await
        .map_err(|_| FenceError::Deadline)?
        .map_err(|_| FenceError::QueueClosed)?;
        vox_types::time::tokio::timeout(remaining()?, reached_rx)
            .await
            .map_err(|_| FenceError::Deadline)?
            .map_err(|_| FenceError::QueueClosed)?
            .map_err(|_| FenceError::RejectedBatch)?;
        // A ready future may win the timer race after a scheduling delay.
        // Never report successful fencing once the absolute budget expired.
        remaining()?;
        Ok(())
    }

    pub(crate) fn outbound_queue_stats(&self) -> (usize, usize) {
        let capacity = self.outbound_tx.max_capacity();
        let available = self.outbound_tx.capacity();
        (capacity.saturating_sub(available), capacity)
    }

    /// Register a CLOSED open-gate for a freshly-allocated outbound channel. Called by
    /// the channel binder at allocation time (during a Call's arg-encode), BEFORE the
    /// Tx sink is bound — so a concurrently-parked `tx.send` that wakes on the bind
    /// finds the gate and waits. Idempotent. (`r[impl rpc.channel.item]`)
    pub(crate) fn register_channel_gate(
        &self,
        lane_id: LaneId,
        generation: &Arc<LaneLifetime>,
        channel_id: vox_types::ChannelId,
    ) -> Arc<ChannelDeclarationGate> {
        let mut gates = self
            .channel_gates
            .lock()
            .expect("channel gates mutex poisoned");
        gates.retain(|_, gate| gate.strong_count() != 0);
        let gate = if let Some(gate) = gates
            .get(&(lane_id, Arc::as_ptr(generation) as usize, channel_id))
            .and_then(std::sync::Weak::upgrade)
        {
            gate
        } else {
            let gate = Arc::new(ChannelDeclarationGate::new(generation.clone()));
            gates.insert(
                (lane_id, Arc::as_ptr(generation) as usize, channel_id),
                Arc::downgrade(&gate),
            );
            gate
        };
        drop(gates);
        DeclarationCapture::record(self, lane_id, generation, &gate);
        gate
    }

    /// Open the gates for `channels` (the channels a just-enqueued Call declared),
    /// releasing any parked channel-item sends so they reach the wire AFTER the Call.
    #[cfg(test)]
    fn open_channel_gates(
        &self,
        lane_id: LaneId,
        generation: &Arc<LaneLifetime>,
        channels: &[vox_types::ChannelId],
    ) {
        if channels.is_empty() {
            return;
        }
        let mut gates = self
            .channel_gates
            .lock()
            .expect("channel gates mutex poisoned");
        for id in channels {
            if let Some(gate) = gates
                .remove(&(lane_id, Arc::as_ptr(generation) as usize, *id))
                .and_then(|gate| gate.upgrade())
            {
                gate.finish(GateState::Open);
            }
        }
    }

    /// Wait until channel `channel_id`'s declaring Call has been enqueued (its gate is
    /// open), if a gate exists. No gate (or an already-open one) returns immediately —
    /// so a non-channel item, or an item whose Call already went out, never blocks.
    async fn await_channel_gate(
        &self,
        lane_id: LaneId,
        generation: Option<&Arc<LaneLifetime>>,
        channel_id: vox_types::ChannelId,
    ) -> Result<(), ()> {
        let Some(generation) = generation else {
            return Ok(());
        };
        let gate = {
            let gates = self
                .channel_gates
                .lock()
                .expect("channel gates mutex poisoned");
            match gates.get(&(lane_id, Arc::as_ptr(generation) as usize, channel_id)) {
                Some(gate) => match gate.upgrade() {
                    Some(gate) => gate,
                    None => return Ok(()),
                },
                None => return Ok(()),
            }
        };
        gate.wait().await
    }

    /// Whether channel `channel_id` may send now: true if it has no gate (not a
    /// locally-opened, not-yet-declared channel) or its gate is already open.
    #[cfg(test)]
    fn channel_gate_open(
        &self,
        lane_id: LaneId,
        generation: &Arc<LaneLifetime>,
        channel_id: vox_types::ChannelId,
    ) -> bool {
        self.channel_gate_state(lane_id, Some(generation), channel_id) == GateState::Open
    }

    fn channel_gate_state(
        &self,
        lane_id: LaneId,
        generation: Option<&Arc<LaneLifetime>>,
        channel_id: vox_types::ChannelId,
    ) -> GateState {
        let Some(generation) = generation else {
            return GateState::Open;
        };
        self.channel_gates
            .lock()
            .expect("channel gates mutex poisoned")
            .get(&(lane_id, Arc::as_ptr(generation) as usize, channel_id))
            .and_then(std::sync::Weak::upgrade)
            .map_or(GateState::Open, |gate| gate.state())
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_outbound_batch<'a>(
        &self,
        mut msg: Message<'a>,
        binder: Option<&'a dyn vox_types::ChannelBinder>,
        forwarded_schemas: Option<&vox_types::SchemaRecvTracker>,
        channel_method: Option<&'static vox_types::MethodDescriptor>,
        extra_schema_sends: Vec<PendingSchemaSend>,
        lane_closed: Option<watch::Receiver<Option<ConnectionCloseReason>>>,
        lane_generation: Option<Arc<LaneLifetime>>,
    ) -> Result<PreparedOutboundBatch, PrepareOutboundError> {
        let conn_id = msg.lane_id;
        let capture = DeclarationCapture::begin(self, conn_id, lane_generation.as_ref());
        let mut incoming_response = None;
        let outgoing_call = match &msg.payload {
            MessagePayload::RequestMessage(RequestMessage {
                id,
                body: RequestBody::Call(call),
            }) => Some((*id, call.method_id)),
            _ => None,
        };
        let (request_id, payload_kind) = match &msg.payload {
            MessagePayload::RequestMessage(req) => {
                let kind = match &req.body {
                    RequestBody::Call(_) => "request.call",
                    RequestBody::Response(_) => "request.response",
                    RequestBody::Cancel(_) => "request.cancel",
                };
                (Some(req.id), kind)
            }
            MessagePayload::SchemaMessage(_) => (None, "schema"),
            MessagePayload::ChannelMessage(_) => (None, "channel"),
            MessagePayload::LaneOpen(_) => (None, "connection.open"),
            MessagePayload::LaneAccept(_) => (None, "connection.accept"),
            MessagePayload::LaneReject(_) => (None, "connection.reject"),
            MessagePayload::LaneClose(_) => (None, "connection.close"),
            MessagePayload::ProtocolError(_) => (None, "protocol.error"),
            MessagePayload::Ping(_) => (None, "ping"),
            MessagePayload::Pong(_) => (None, "pong"),
        };
        trace!(
            conn_id = %conn_id,
            ?request_id,
            payload_kind,
            "connection preparing outbound message"
        );
        let (tx, conn_state, schema_sends) = {
            let mut inner = self.inner.lock().expect("connection core mutex poisoned");
            let Some(tx) = inner.tx.clone() else {
                return Err(PrepareOutboundError {
                    gated_channels: Vec::new(),
                    gate_guard: capture.finish(),
                });
            };
            let conn_state = get_or_create_send_conn_state(&mut inner, conn_id);
            drop(inner);

            if let MessagePayload::RequestMessage(req) = &mut msg.payload {
                vox_types::dlog!(
                    "[connection-core] send request: conn={:?} req={:?} body={} forwarded={}",
                    conn_id,
                    req.id,
                    match &req.body {
                        RequestBody::Call(_) => "Call",
                        RequestBody::Response(_) => "Response",
                        RequestBody::Cancel(_) => "Cancel",
                    },
                    forwarded_schemas.is_some()
                );
                let schema_sends = {
                    let mut conn_state_guard =
                        conn_state.lock().expect("send conn state mutex poisoned");
                    let mut schema_sends = extra_schema_sends;
                    match &mut req.body {
                        RequestBody::Call(call) => {
                            if let Some(schema_send) = Self::plan_call_schema_send(
                                &mut conn_state_guard,
                                req.id,
                                call.method_id,
                                call,
                                forwarded_schemas,
                            ) {
                                schema_sends.push(schema_send);
                            }
                            call.schemas = Default::default();
                        }
                        RequestBody::Response(resp) => {
                            if let Some(method_id) =
                                conn_state_guard.inflight_incoming.get(&req.id).copied()
                            {
                                incoming_response = Some((req.id, method_id));
                                if let Some(schema_send) = Self::plan_response_schema_send(
                                    &mut conn_state_guard,
                                    req.id,
                                    method_id,
                                    resp,
                                    forwarded_schemas,
                                ) {
                                    schema_sends.push(schema_send);
                                }
                            }
                            resp.schemas = Default::default();
                        }
                        RequestBody::Cancel(_) => {}
                    }
                    schema_sends
                };
                (tx, conn_state, schema_sends)
            } else {
                (tx, conn_state, extra_schema_sends)
            }
        };
        trace!(
            conn_id = %conn_id,
            ?request_id,
            payload_kind,
            schema_count = schema_sends.len(),
            "connection preparing outbound payload"
        );

        // Out-of-band channel allocation. If this Call's args carry `Tx`/`Rx`
        // handles, pre-encode the args now under a channel collector (with the
        // binder installed) so the allocated `ChannelId`s ride in `call.channels`
        // and each handle goes on the wire as a small index. The args then travel
        // as already-encoded bytes — the schema for them was already attached
        // above from the `Value` shape. r[impl rpc.request] r[impl rpc.channel.allocation]
        // Channels this Call opens, whose send-gates must be opened once the Call has
        // been enqueued (so a concurrent `tx.send` cannot beat the Call to the wire).
        let mut gated_channels: Vec<vox_types::ChannelId> = Vec::new();
        let channel_storage: Option<Vec<u8>> = if let MessagePayload::RequestMessage(req) =
            &msg.payload
            && let RequestBody::Call(call) = &req.body
            && let vox_types::Payload::Value { ptr, shape, .. } = &call.args
            && vox_types::shape_contains_channel(shape)
        {
            let (ptr, shape) = (*ptr, *shape);
            // The binder registers a CLOSED gate per channel it allocates here (before
            // binding the Tx sink), so an app `tx.send` that wakes mid-encode parks.
            let encode_args = || match binder {
                Some(b) => {
                    vox_types::with_channel_binder(b, || vox_phon::to_vec_for_shape(ptr, shape))
                }
                None => vox_phon::to_vec_for_shape(ptr, shape),
            };
            let (encoded, channels) = match channel_method {
                Some(method) => vox_types::collect_channels_for_method(method, encode_args),
                None => vox_types::collect_channels(encode_args),
            };
            gated_channels = channels.clone();
            let encoded = match encoded {
                Ok(encoded) => encoded,
                Err(_) => {
                    return Err(PrepareOutboundError {
                        gated_channels,
                        gate_guard: capture.finish(),
                    });
                }
            };
            if let MessagePayload::RequestMessage(req) = &mut msg.payload
                && let RequestBody::Call(call) = &mut req.body
            {
                call.channels = channels;
            }
            Some(encoded)
        } else {
            None
        };

        let prepared = if let Some(bytes) = &channel_storage {
            // Narrow `msg`'s lifetime to the pre-encoded `bytes` (covariance) and
            // swap the args to the encoded payload, then encode the envelope. The
            // `bytes` outlive `prepare_msg`, which consumes the message synchronously.
            let msg = swap_call_args_to_bytes(msg, bytes);
            tx.clone().prepare_msg(msg, binder)
        } else {
            tx.clone().prepare_msg(msg, binder)
        };
        let payload_send = match prepared {
            Ok(send) => send,
            Err(_) => {
                return Err(PrepareOutboundError {
                    gated_channels,
                    gate_guard: capture.finish(),
                });
            }
        };
        trace!(
            conn_id = %conn_id,
            ?request_id,
            payload_kind,
            "connection prepared outbound payload"
        );

        let (result_tx, result_rx) = oneshot::channel("connection.outbound.result");
        Ok((
            OutboundBatch {
                conn_id,
                lane_closed,
                lane_generation,
                queued_response: None,
                #[cfg(test)]
                before_worker_write: self.before_worker_write.clone(),
                request_id,
                outgoing_call,
                incoming_response,
                payload_kind,
                conn_state,
                tx,
                schema_sends,
                payload_send,
                result_tx,
            },
            result_rx,
            gated_channels,
            capture.finish(),
        ))
    }

    // r[impl schema.principles.sender-driven]
    pub(crate) async fn send<'a>(
        &self,
        msg: Message<'a>,
        binder: Option<&'a dyn vox_types::ChannelBinder>,
        forwarded_schemas: Option<&vox_types::SchemaRecvTracker>,
    ) -> Result<(), ()> {
        self.send_with_options(
            msg,
            binder,
            forwarded_schemas,
            None,
            Vec::new(),
            |_| {},
            None,
            None,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn send_with_options<'a>(
        &self,
        msg: Message<'a>,
        binder: Option<&'a dyn vox_types::ChannelBinder>,
        forwarded_schemas: Option<&vox_types::SchemaRecvTracker>,
        channel_method: Option<&'static vox_types::MethodDescriptor>,
        extra_schema_sends: Vec<PendingSchemaSend>,
        declared_channels: impl FnOnce(&[vox_types::ChannelId]),
        lane_closed: Option<watch::Receiver<Option<ConnectionCloseReason>>>,
        lane_generation: Option<Arc<LaneLifetime>>,
        response_reservation: Option<response_reservation::ResponseReservation>,
    ) -> Result<(), ()> {
        let lane_id = msg.lane_id;
        if self.admission_is_stopped()
            || lane_closed
                .as_ref()
                .is_some_and(|closed| closed.borrow().is_some())
        {
            declared_channels(&[]);
            return Err(());
        }
        // r[impl rpc.channel.item] Hold an outbound channel item/close until the Call
        // that opened its channel has been enqueued — the sender upholds frame order.
        if let Some(channel_id) = gated_channel_id(&msg) {
            let gate = self
                .await_channel_gate(lane_id, lane_generation.as_ref(), channel_id)
                .fuse();
            let stopped = self.wait_admission_stopped().fuse();
            let lane_stopped = Self::wait_lane_closed(&lane_closed).fuse();
            futures_util::pin_mut!(gate, stopped, lane_stopped);
            futures_util::select_biased! {
                () = stopped => { declared_channels(&[]); return Err(()); },
                () = lane_stopped => { declared_channels(&[]); return Err(()); },
                result = gate => { if result.is_err() { declared_channels(&[]); return Err(()); } },
            }
        }
        let (batch, result_rx, gated_channels, gate_guard) = match self.prepare_outbound_batch(
            msg,
            binder,
            forwarded_schemas,
            channel_method,
            extra_schema_sends,
            lane_closed.clone(),
            lane_generation.clone(),
        ) {
            Ok(prepared) => prepared,
            Err(err) => {
                let _gate_guard = err.gate_guard;
                declared_channels(&err.gated_channels);
                return Err(());
            }
        };
        let mut batch = Box::new(batch);
        declared_channels(&gated_channels);
        let queued = async {
            let reserve = self.outbound_tx.clone().reserve_owned().fuse();
            let stopped = self.wait_admission_stopped().fuse();
            let lane_stopped = Self::wait_lane_closed(&lane_closed).fuse();
            futures_util::pin_mut!(reserve, stopped, lane_stopped);
            let permit = futures_util::select_biased! {
                () = stopped => return Err(()),
                () = lane_stopped => return Err(()),
                permit = reserve => permit.map_err(|_| ())?,
            };
            let admission = self.admission.lock().fuse();
            let stopped = self.wait_admission_stopped().fuse();
            let lane_stopped = Self::wait_lane_closed(&lane_closed).fuse();
            futures_util::pin_mut!(admission, stopped, lane_stopped);
            let guard = futures_util::select_biased! {
                () = stopped => return Err(()),
                () = lane_stopped => return Err(()),
                guard = admission => guard,
            };
            if *guard
                || self.admission_is_stopped()
                || lane_closed
                    .as_ref()
                    .is_some_and(|closed| closed.borrow().is_some())
            {
                return Err(());
            }
            // A forced stop can race after this check and before enqueue. Such
            // a preadmitted batch has an uncertain outcome, not a hard cutoff.
            #[cfg(test)]
            if let Some(hook) = self.before_admission_commit.lock().unwrap().take() {
                hook();
            }
            if let Some(reservation) = response_reservation {
                let generation = lane_generation.as_ref().ok_or(())?;
                if !reservation.belongs_to_lane(generation) {
                    return Err(());
                }
                reservation
                    .commit_with(|queued| {
                        batch.queued_response = Some(queued);
                        drop(permit.send(OutboundCommand::Batch(batch)));
                    })
                    .map_err(|_| ())?;
            } else {
                let enqueue = || drop(permit.send(OutboundCommand::Batch(batch)));
                if let Some(generation) = lane_generation.as_ref() {
                    generation.commit_admission(enqueue).map_err(|_| ())?;
                } else {
                    enqueue();
                }
            }
            Ok::<(), ()>(())
        }
        .await;
        if queued.is_err() {
            if let Some(observer) = &self.observer {
                observer.driver_event(vox_types::DriverEvent::OutboundQueueClosed { lane_id });
            }
            return Err(());
        }
        // No cancellation point occurs between successful enqueue and opening.
        gate_guard.open();
        trace!(conn_id = %lane_id, "connection queued outbound batch");
        let result = result_rx.await.map_err(|_| ());
        trace!(
            conn_id = %lane_id,
            ok = result.as_ref().map(|inner| inner.is_ok()).unwrap_or(false),
            "connection outbound batch completed"
        );
        match result? {
            Ok(()) => Ok(()),
            Err(_) => {
                if let Some(observer) = &self.observer {
                    observer.driver_event(vox_types::DriverEvent::EncodeError {
                        lane_id,
                        kind: vox_types::EncodeErrorKind::Transport,
                    });
                }
                Err(())
            }
        }
    }

    // r[impl rpc.flow-control.credit.try-send]
    #[allow(clippy::too_many_arguments)]
    fn try_send_with_options<'a>(
        &self,
        msg: Message<'a>,
        binder: Option<&'a dyn vox_types::ChannelBinder>,
        forwarded_schemas: Option<&vox_types::SchemaRecvTracker>,
        channel_method: Option<&'static vox_types::MethodDescriptor>,
        extra_schema_sends: Vec<PendingSchemaSend>,
        lane_closed: Option<watch::Receiver<Option<ConnectionCloseReason>>>,
        lane_generation: Option<Arc<LaneLifetime>>,
    ) -> Result<(), TrySendError<()>> {
        let lane_id = msg.lane_id;
        if self.admission_is_stopped()
            || lane_closed
                .as_ref()
                .is_some_and(|closed| closed.borrow().is_some())
        {
            return Err(TrySendError::Closed(()));
        }
        // r[impl rpc.channel.item] A channel item whose declaring Call hasn't been
        // enqueued yet can't go out (frame order); signal backpressure so the caller
        // retries — the async `send` path parks instead.
        if let Some(channel_id) = gated_channel_id(&msg) {
            match self.channel_gate_state(lane_id, lane_generation.as_ref(), channel_id) {
                GateState::Open => {}
                GateState::Pending => return Err(TrySendError::Full(())),
                GateState::Failed => return Err(TrySendError::Closed(())),
            }
        }
        let (batch, _result_rx, _gated_channels, gate_guard) = match self.prepare_outbound_batch(
            msg,
            binder,
            forwarded_schemas,
            channel_method,
            extra_schema_sends,
            lane_closed.clone(),
            lane_generation.clone(),
        ) {
            Ok(prepared) => prepared,
            Err(err) => {
                let _gate_guard = err.gate_guard;
                return Err(TrySendError::Closed(()));
            }
        };
        let batch = Box::new(batch);
        let guard = match self.admission.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                return Err(
                    if self.admission_is_stopped()
                        || lane_closed
                            .as_ref()
                            .is_some_and(|closed| closed.borrow().is_some())
                    {
                        TrySendError::Closed(())
                    } else {
                        TrySendError::Full(())
                    },
                );
            }
        };
        if *guard
            || self.admission_is_stopped()
            || lane_closed
                .as_ref()
                .is_some_and(|closed| closed.borrow().is_some())
        {
            drop(guard);
            return Err(TrySendError::Closed(()));
        }
        // As above, a forced stop can overlap this already-admitted try-send.
        #[cfg(test)]
        if let Some(hook) = self.before_admission_commit.lock().unwrap().take() {
            hook();
        }
        let enqueue = || self.outbound_tx.try_send(OutboundCommand::Batch(batch));
        let enqueued = if let Some(generation) = lane_generation.as_ref() {
            match generation.commit_admission(enqueue) {
                Ok(result) => result,
                Err(_) => return Err(TrySendError::Closed(())),
            }
        } else {
            enqueue()
        };
        let result = enqueued.map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => {
                if let Some(observer) = &self.observer {
                    observer.driver_event(vox_types::DriverEvent::OutboundQueueFull { lane_id });
                }
                TrySendError::Full(())
            }
            mpsc::error::TrySendError::Closed(_) => {
                if let Some(observer) = &self.observer {
                    observer.driver_event(vox_types::DriverEvent::OutboundQueueClosed { lane_id });
                }
                TrySendError::Closed(())
            }
        });
        drop(guard);
        if result.is_ok() {
            gate_guard.open();
        }
        result
    }

    /// Record that an incoming call was received, so we can look up the
    /// method_id when sending the response.
    pub(crate) fn record_incoming_call(
        &self,
        conn_id: LaneId,
        request_id: RequestId,
        method_id: vox_types::MethodId,
    ) -> bool {
        let mut inner = self.inner.lock().expect("connection core mutex poisoned");
        let conn_state = get_or_create_send_conn_state(&mut inner, conn_id);
        vox_types::dlog!(
            "[schema] record_incoming_call: conn={:?} req={:?} method={:?}",
            conn_id,
            request_id,
            method_id
        );
        let mut state = conn_state.lock().expect("send conn state mutex poisoned");
        if let std::collections::hash_map::Entry::Vacant(entry) =
            state.inflight_incoming.entry(request_id)
        {
            entry.insert(method_id);
            true
        } else {
            false
        }
    }

    pub(crate) fn take_outgoing_call_method(
        &self,
        conn_id: LaneId,
        request_id: RequestId,
    ) -> Option<vox_types::MethodId> {
        let inner = self.inner.lock().expect("connection core mutex poisoned");
        inner.conns.get(&conn_id).and_then(|conn_state| {
            conn_state
                .lock()
                .expect("send conn state mutex poisoned")
                .inflight_outgoing
                .remove(&request_id)
        })
    }

    pub(crate) fn prepare_response_for_method(
        &self,
        conn_id: LaneId,
        _request_id: RequestId,
        method_id: vox_types::MethodId,
        response: &mut RequestResponse<'_>,
    ) {
        let mut inner = self.inner.lock().expect("connection core mutex poisoned");
        let conn_state = get_or_create_send_conn_state(&mut inner, conn_id);
        let conn_state = conn_state.lock().expect("send conn state mutex poisoned");
        if conn_state
            .send_tracker
            .has_sent_binding(method_id, vox_types::BindingDirection::Response)
        {
            response.schemas = Default::default();
            return;
        }

        let prepared = match &response.ret {
            vox_types::Payload::Value { shape, .. } => {
                match vox_types::SchemaSendTracker::plan_for_shape(shape) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        tracing::error!("response schema extraction failed: {error}");
                        return;
                    }
                }
            }
            vox_types::Payload::Encoded(_) => {
                tracing::error!(
                    "schema attachment failed: missing forwarded response schemas for method {:?}",
                    method_id
                );
                return;
            }
        };
        response.schemas = prepared.to_payload();
    }

    /// Attach the method's response schema for an explicit wire `shape` (the
    /// erased-error-response path). The outbound worker commits the schema only
    /// after its ordered local send succeeds (`r[schema.exchange]`).
    pub(crate) fn prepare_response_for_shape(
        &self,
        conn_id: LaneId,
        _request_id: RequestId,
        method_id: vox_types::MethodId,
        shape: &'static Shape,
        response: &mut RequestResponse<'_>,
    ) {
        let mut inner = self.inner.lock().expect("connection core mutex poisoned");
        let conn_state = get_or_create_send_conn_state(&mut inner, conn_id);
        let conn_state = conn_state.lock().expect("send conn state mutex poisoned");
        if conn_state
            .send_tracker
            .has_sent_binding(method_id, vox_types::BindingDirection::Response)
        {
            response.schemas = Default::default();
            return;
        }
        match vox_types::SchemaSendTracker::plan_for_shape(shape) {
            Ok(prepared) => response.schemas = prepared.to_payload(),
            Err(e) => tracing::error!("error-response schema extraction failed: {e}"),
        }
    }

    fn get_or_plan_binding_for_shape(
        conn_state: &mut SendConnState,
        key: (vox_types::BindingDirection, vox_types::MethodId),
        request_id: RequestId,
        kind: &str,
        shape: &'static Shape,
    ) -> Option<vox_types::PreparedSchemaPlan> {
        if let Some(prepared) = conn_state.planned_bindings.get(&key) {
            return Some(prepared.clone());
        }
        match vox_types::SchemaSendTracker::plan_for_shape(shape) {
            Ok(prepared) => {
                vox_types::dlog!(
                    "[schema] planned {} {} schemas for method {:?} (req {:?})",
                    prepared.bytes.len(),
                    kind,
                    key.1,
                    request_id
                );
                conn_state.planned_bindings.insert(key, prepared.clone());
                Some(prepared)
            }
            Err(e) => {
                tracing::error!("schema extraction failed: {e}");
                None
            }
        }
    }

    /// Forward a binding's schema for the proxy/relay path: source the peer's phon
    /// schema-closure bytes from the receive tracker (where they were stored when the
    /// upstream sent them) and re-send them verbatim. `None` if not received yet.
    fn get_or_plan_binding_from_tracker(
        conn_state: &mut SendConnState,
        key: (vox_types::BindingDirection, vox_types::MethodId),
        tracker: &vox_types::SchemaRecvTracker,
    ) -> Option<vox_types::PreparedSchemaPlan> {
        if let Some(prepared) = conn_state.planned_bindings.get(&key) {
            return Some(prepared.clone());
        }
        let bytes = tracker.writer_schema_bytes(key.1, key.0)?;
        let prepared = vox_types::PreparedSchemaPlan { bytes };
        conn_state.planned_bindings.insert(key, prepared.clone());
        Some(prepared)
    }

    fn plan_response_schema_send(
        conn_state: &mut SendConnState,
        _request_id: RequestId,
        method_id: vox_types::MethodId,
        response: &mut RequestResponse<'_>,
        forwarded_schemas: Option<&vox_types::SchemaRecvTracker>,
    ) -> Option<PendingSchemaSend> {
        // r[impl schema.exchange.callee]
        if conn_state
            .send_tracker
            .has_sent_binding(method_id, vox_types::BindingDirection::Response)
        {
            response.schemas = Default::default();
            return None;
        }

        let key = (vox_types::BindingDirection::Response, method_id);
        let prepared = if !response.schemas.is_empty() {
            // The response already carries its phon schema closure (forwarded from
            // upstream, or set by an earlier stage) — re-send it verbatim.
            vox_types::PreparedSchemaPlan {
                bytes: response.schemas.0.clone(),
            }
        } else {
            match &response.ret {
                vox_types::Payload::Value { shape, .. } => {
                    match vox_types::SchemaSendTracker::plan_for_shape(shape) {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            tracing::error!("response schema extraction failed: {error}");
                            return None;
                        }
                    }
                }
                vox_types::Payload::Encoded(_) => {
                    let Some(source) = forwarded_schemas else {
                        tracing::error!(
                            "schema attachment failed: missing forwarded response schemas for method {:?}",
                            method_id
                        );
                        return None;
                    };
                    vox_types::PreparedSchemaPlan {
                        bytes: source.writer_schema_bytes(key.1, key.0)?,
                    }
                }
            }
        };

        Some(PendingSchemaSend {
            method_id,
            direction: vox_types::BindingDirection::Response,
            prepared,
        })
    }

    fn plan_call_schema_send(
        conn_state: &mut SendConnState,
        request_id: RequestId,
        method_id: vox_types::MethodId,
        call: &mut vox_types::RequestCall<'_>,
        forwarded_schemas: Option<&vox_types::SchemaRecvTracker>,
    ) -> Option<PendingSchemaSend> {
        // r[impl schema.exchange.caller]
        if conn_state
            .send_tracker
            .has_sent_binding(method_id, vox_types::BindingDirection::Args)
        {
            call.schemas = Default::default();
            return None;
        }

        let key = (vox_types::BindingDirection::Args, method_id);
        let prepared = if !call.schemas.is_empty() {
            conn_state
                .planned_bindings
                .get(&key)
                .cloned()
                .unwrap_or_else(|| vox_types::PreparedSchemaPlan {
                    bytes: call.schemas.0.clone(),
                })
        } else {
            match &call.args {
                vox_types::Payload::Value { shape, .. } => {
                    Self::get_or_plan_binding_for_shape(conn_state, key, request_id, "args", shape)?
                }
                vox_types::Payload::Encoded(_) => {
                    let Some(source) = forwarded_schemas else {
                        tracing::error!(
                            "schema attachment failed: missing forwarded args schemas for method {:?}",
                            method_id
                        );
                        return None;
                    };
                    Self::get_or_plan_binding_from_tracker(conn_state, key, source)?
                }
            }
        };

        Some(PendingSchemaSend {
            method_id,
            direction: vox_types::BindingDirection::Args,
            prepared,
        })
    }
}

pub trait DynConduitTx: MaybeSend + MaybeSync {
    fn abort(&self) {}
    fn send_success_is_locally_settled(&self) -> bool {
        false
    }
    fn close_shared_bounded(self: Arc<Self>, _budget: Duration) -> OutboundSend {
        Box::pin(std::future::ready(Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "shared bounded close is not supported by this conduit",
        ))))
    }
    fn prepare_msg<'a>(
        self: Arc<Self>,
        msg: Message<'a>,
        binder: Option<&'a dyn vox_types::ChannelBinder>,
    ) -> std::io::Result<OutboundSend>;
}
pub trait DynConduitRx: MaybeSend {
    fn recv_msg<'a>(&'a mut self)
    -> BoxFut<'a, std::io::Result<Option<SelfRef<Message<'static>>>>>;

    /// Descriptors that arrived with the frame from the most recent
    /// `recv_msg`. Threaded alongside the message to the typed-decode site.
    fn take_frame_fds(&mut self) -> vox_types::FrameFds;
}

// r[impl connection.message]
impl<T> DynConduitTx for T
where
    T: ConduitTx<Msg = MessageFamily> + MaybeSend + MaybeSync + 'static,
{
    fn send_success_is_locally_settled(&self) -> bool {
        ConduitTx::send_success_is_locally_settled(self)
    }
    fn abort(&self) {
        ConduitTx::abort(self);
    }
    fn close_shared_bounded(self: Arc<Self>, budget: Duration) -> OutboundSend {
        Box::pin(async move { ConduitTx::close_shared_bounded(&*self, budget).await })
    }
    fn prepare_msg<'a>(
        self: Arc<Self>,
        msg: Message<'a>,
        binder: Option<&'a dyn vox_types::ChannelBinder>,
    ) -> std::io::Result<OutboundSend> {
        let prepared = if let Some(binder) = binder {
            vox_types::with_channel_binder(binder, || self.prepare_send(msg))
        } else {
            self.prepare_send(msg)
        };
        let prepared = prepared.map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(Box::pin(async move {
            self.send_prepared(prepared)
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))
        }))
    }
}

impl<T> DynConduitRx for T
where
    T: ConduitRx<Msg = MessageFamily> + MaybeSend,
{
    fn recv_msg<'a>(
        &'a mut self,
    ) -> BoxFut<'a, std::io::Result<Option<SelfRef<Message<'static>>>>> {
        Box::pin(async move {
            self.recv()
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))
        })
    }

    fn take_frame_fds(&mut self) -> vox_types::FrameFds {
        ConduitRx::take_frame_fds(self)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use vox_rt::sync::mpsc;
    use vox_types::{
        Backing, BindingDirection, Conduit, DriverEvent, HandshakeResult, LaneAccept, LaneReject,
        Payload, RequestCall, SelfRef, TransportEvent, VoxObserverHandle,
    };

    use super::*;

    #[derive(Clone)]
    struct CapturingTx {
        sent: Arc<Mutex<Vec<CapturedMessage>>>,
    }

    #[derive(Debug)]
    struct CapturedMessage {
        lane_id: LaneId,
        payload: CapturedPayload,
    }

    #[derive(Debug)]
    enum CapturedPayload {
        Schema {
            method_id: vox_types::MethodId,
            direction: BindingDirection,
            schemas: Vec<u8>,
        },
        Call {
            request_id: RequestId,
            method_id: vox_types::MethodId,
            schemas_len: usize,
        },
        Response {
            request_id: RequestId,
            schemas_len: usize,
        },
        Other,
    }

    impl ConduitTx for CapturingTx {
        type Error = std::io::Error;
        type Msg = MessageFamily;
        type Prepared = CapturedMessage;

        fn prepare_send(&self, item: Message<'_>) -> Result<Self::Prepared, Self::Error> {
            let payload = match &item.payload {
                MessagePayload::SchemaMessage(schema) => CapturedPayload::Schema {
                    method_id: schema.method_id,
                    direction: schema.direction,
                    schemas: schema.schemas.0.clone(),
                },
                MessagePayload::RequestMessage(request) => match &request.body {
                    RequestBody::Call(call) => CapturedPayload::Call {
                        request_id: request.id,
                        method_id: call.method_id,
                        schemas_len: call.schemas.0.len(),
                    },
                    RequestBody::Response(response) => CapturedPayload::Response {
                        request_id: request.id,
                        schemas_len: response.schemas.0.len(),
                    },
                    _ => CapturedPayload::Other,
                },
                _ => CapturedPayload::Other,
            };
            Ok(CapturedMessage {
                lane_id: item.lane_id,
                payload,
            })
        }

        async fn send_prepared(&self, prepared: Self::Prepared) -> Result<(), Self::Error> {
            self.sent
                .lock()
                .expect("captured message mutex poisoned")
                .push(prepared);
            Ok(())
        }

        async fn close(self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // This test conduit completes its local write synchronously before its
    // send future returns. It says nothing about peer processing.
    struct SettledCapturingTx(CapturingTx);

    impl ConduitTx for SettledCapturingTx {
        type Error = std::io::Error;
        type Msg = MessageFamily;
        type Prepared = CapturedMessage;

        fn send_success_is_locally_settled(&self) -> bool {
            true
        }

        fn prepare_send(&self, item: Message<'_>) -> Result<Self::Prepared, Self::Error> {
            self.0.prepare_send(item)
        }

        async fn send_prepared(&self, prepared: Self::Prepared) -> Result<(), Self::Error> {
            self.0.send_prepared(prepared).await
        }

        async fn close(self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct PendingRx;

    #[tokio::test]
    async fn unsupported_type_erased_conduit_does_not_claim_shared_close() {
        let shared: Arc<dyn DynConduitTx> = Arc::new(CapturingTx {
            sent: Arc::new(Mutex::new(Vec::new())),
        });
        assert!(!shared.send_success_is_locally_settled());
        let error = shared
            .close_shared_bounded(Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);

        use vox_types::Conduit;
        let (link, _peer) = crate::memory_link_pair(4);
        let (tx, _rx) = crate::BareConduit::<MessageFamily, _>::new(link).split();
        let shared: Arc<dyn DynConduitTx> = Arc::new(tx);
        assert!(shared.send_success_is_locally_settled());
        let error = shared
            .close_shared_bounded(Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    }

    impl ConduitRx for PendingRx {
        type Error = std::io::Error;
        type Msg = MessageFamily;

        async fn recv(&mut self) -> Result<Option<SelfRef<Message<'static>>>, Self::Error> {
            std::future::pending().await
        }
    }

    struct RecordingObserver {
        driver_events: Arc<Mutex<Vec<DriverEvent>>>,
        transport_events: Arc<Mutex<Vec<TransportEvent>>>,
    }

    impl vox_types::VoxObserver for RecordingObserver {
        fn driver_event(&self, event: DriverEvent) {
            self.driver_events
                .lock()
                .expect("driver events mutex poisoned")
                .push(event);
        }

        fn transport_event(&self, event: TransportEvent) {
            self.transport_events
                .lock()
                .expect("transport events mutex poisoned")
                .push(event);
        }
    }

    fn make_connection() -> Connection {
        let (a, b) = crate::memory_link_pair(32);
        // Keep the peer link alive so connection-core sends don't fail with broken pipe.
        std::mem::forget(b);
        let conduit = crate::BareConduit::new(a);
        let (tx, rx) = conduit.split();
        let (_open_tx, open_rx) = mpsc::channel::<OpenRequest>("connection.open.test", 4);
        let (_close_tx, close_rx) = mpsc::channel::<CloseRequest>("connection.close.test", 4);
        let (control_tx, control_rx) = mpsc::unbounded_channel("connection.control.test");
        Connection::pre_handshake(
            tx, rx, None, open_rx, close_rx, control_tx, control_rx, None, None,
        )
    }

    fn make_connection_with_observer(observer: VoxObserverHandle) -> Connection {
        let (a, b) = crate::memory_link_pair(32);
        std::mem::forget(b);
        let conduit = crate::BareConduit::new(a);
        let (tx, rx) = conduit.split();
        let (_open_tx, open_rx) = mpsc::channel::<OpenRequest>("connection.open.observed.test", 4);
        let (_close_tx, close_rx) =
            mpsc::channel::<CloseRequest>("connection.close.observed.test", 4);
        let (control_tx, control_rx) = mpsc::unbounded_channel("connection.control.observed.test");
        Connection::pre_handshake(
            tx,
            rx,
            None,
            open_rx,
            close_rx,
            control_tx,
            control_rx,
            None,
            Some(observer),
        )
    }

    fn make_capturing_connection(
        sent: Arc<Mutex<Vec<CapturedMessage>>>,
    ) -> (Connection, LaneHandle) {
        let (_open_tx, open_rx) = mpsc::channel::<OpenRequest>("connection.open.capture.test", 4);
        let (_close_tx, close_rx) =
            mpsc::channel::<CloseRequest>("connection.close.capture.test", 4);
        let (control_tx, control_rx) = mpsc::unbounded_channel("connection.control.capture.test");
        let mut connection = Connection::pre_handshake(
            CapturingTx { sent },
            PendingRx,
            None,
            open_rx,
            close_rx,
            control_tx,
            control_rx,
            None,
            None,
        );
        let handle = connection
            .establish_from_handshake(test_handshake(
                ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 64,
                    initial_channel_credit: 16,
                },
                ConnectionSettings {
                    parity: Parity::Even,
                    max_concurrent_requests: 64,
                    initial_channel_credit: 16,
                },
            ))
            .expect("establish captured connection");
        (connection, handle)
    }

    fn make_settled_capturing_connection(
        sent: Arc<Mutex<Vec<CapturedMessage>>>,
    ) -> (Connection, LaneHandle) {
        let (_open_tx, open_rx) = mpsc::channel("settled.open", 4);
        let (_close_tx, close_rx) = mpsc::channel("settled.close", 4);
        let (control_tx, control_rx) = mpsc::unbounded_channel("settled.control");
        let mut connection = Connection::pre_handshake(
            SettledCapturingTx(CapturingTx { sent }),
            PendingRx,
            None,
            open_rx,
            close_rx,
            control_tx,
            control_rx,
            None,
            None,
        );
        let settings = ConnectionSettings {
            parity: Parity::Odd,
            max_concurrent_requests: 1,
            initial_channel_credit: 1,
        };
        let handle = connection
            .establish_from_handshake(test_handshake(settings.clone(), settings))
            .unwrap();
        (connection, handle)
    }

    fn test_handshake(
        our_settings: ConnectionSettings,
        peer_settings: ConnectionSettings,
    ) -> HandshakeResult {
        HandshakeResult {
            role: ConnectionRole::Initiator,
            our_settings,
            peer_settings,
            our_schema: vec![],
            peer_schema: vec![],
            peer_metadata: vox_types::Metadata::default(),
            peer_evidence: vox_types::PeerEvidence::none(),
            peer_identity: vox_types::PeerIdentity::anonymous(),
        }
    }

    fn accept_ref() -> SelfRef<LaneAccept> {
        SelfRef::owning(
            Backing::Boxed(Box::<[u8]>::default()),
            LaneAccept {
                connection_settings: ConnectionSettings {
                    parity: Parity::Even,
                    max_concurrent_requests: 64,
                    initial_channel_credit: 16,
                },
                metadata: vox_types::Metadata::default(),
            },
        )
    }

    fn zero_credit_accept_ref() -> SelfRef<LaneAccept> {
        SelfRef::owning(
            Backing::Boxed(Box::<[u8]>::default()),
            LaneAccept {
                connection_settings: ConnectionSettings {
                    parity: Parity::Even,
                    max_concurrent_requests: 64,
                    initial_channel_credit: 0,
                },
                metadata: vox_types::Metadata::default(),
            },
        )
    }

    fn reject_ref() -> SelfRef<LaneReject> {
        SelfRef::owning(
            Backing::Boxed(Box::<[u8]>::default()),
            LaneReject {
                metadata: vox_types::Metadata::default(),
            },
        )
    }

    fn hardened_test_limits() -> ConnectionResourceLimits {
        ConnectionResourceLimits {
            max_service_lanes: 1,
            max_peer_lane_opens: 2,
            max_inbound_requests_per_lane: 64,
            max_initial_channel_credit: 16,
            max_peer_channel_credit: 64,
            max_peer_concurrent_requests: 64,
            max_peer_calls_per_lane: 64,
            max_peer_channel_ids_per_lane: 2,
            max_schemas_per_lane: 2,
            max_schema_bytes_per_lane: 4,
            max_message_processing_time: Duration::from_secs(30),
        }
    }

    fn inbound_open(credit: u32, requests: u32) -> SelfRef<LaneOpen> {
        SelfRef::owning(
            Backing::Boxed(Box::default()),
            LaneOpen {
                connection_settings: ConnectionSettings {
                    parity: Parity::Even,
                    max_concurrent_requests: requests,
                    initial_channel_credit: credit,
                },
                metadata: vox_types::metadata().str("vox-service", "test").build(),
            },
        )
    }

    struct RetainLanes(Arc<Mutex<Vec<LaneHandle>>>);
    impl LaneAcceptor for RetainLanes {
        fn accept(&self, _: &LaneRequest, pending: PendingLane) -> Result<(), LaneRejection> {
            self.0.lock().unwrap().push(pending.into_handle());
            Ok(())
        }
    }

    // r[verify lane.open.wire.rejection]
    // r[verify lane.open.settings]
    #[tokio::test]
    async fn resource_limits_bound_lane_flood_and_use_local_settings() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (mut connection, _control) = make_capturing_connection(sent.clone());
        let lanes = Arc::new(Mutex::new(Vec::new()));
        connection.lane_acceptor = Some(Arc::new(RetainLanes(lanes.clone())));
        connection.resource_limits = Some(hardened_test_limits());
        connection.local_connection_settings.initial_channel_credit = 2;
        connection.local_connection_settings.max_concurrent_requests = 3;

        connection
            .handle_inbound_open(LaneId(2), inbound_open(32, 40))
            .await;
        let guard = lanes.lock().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard[0].local_settings.initial_channel_credit, 2);
        assert_eq!(guard[0].local_settings.max_concurrent_requests, 3);
        drop(guard);
        connection
            .handle_inbound_open(LaneId(4), inbound_open(32, 40))
            .await;
        assert!(!connection.conns.contains_key(&LaneId(4)));
        let _ = connection.remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local);
        connection
            .handle_inbound_open(LaneId(6), inbound_open(32, 40))
            .await;
        assert!(connection.resource_exhausted);
        assert!(!connection.conns.contains_key(&LaneId(6)));
        assert_eq!(sent.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn oversized_peer_lane_settings_are_rejected_before_activation() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (mut connection, _control) = make_capturing_connection(sent.clone());
        let lanes = Arc::new(Mutex::new(Vec::new()));
        connection.lane_acceptor = Some(Arc::new(RetainLanes(lanes.clone())));
        connection.resource_limits = Some(hardened_test_limits());
        connection
            .handle_inbound_open(LaneId(2), inbound_open(u32::MAX, 1))
            .await;
        connection
            .handle_inbound_open(LaneId(4), inbound_open(1, u32::MAX))
            .await;
        assert!(lanes.lock().unwrap().is_empty());
        assert!(!connection.conns.contains_key(&LaneId(2)));
        assert!(!connection.conns.contains_key(&LaneId(4)));
        assert_eq!(sent.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn shutdown_fences_admitted_write_before_shared_close() {
        struct GatedCloseTx {
            started: Mutex<Option<oneshot::Sender<()>>>,
            release: Mutex<Option<oneshot::Receiver<()>>>,
            events: Arc<Mutex<Vec<&'static str>>>,
            aborted: Arc<std::sync::atomic::AtomicBool>,
        }
        impl ConduitTx for GatedCloseTx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            type Prepared = ();
            fn prepare_send(&self, _: Message<'_>) -> Result<(), Self::Error> {
                Ok(())
            }
            fn send_success_is_locally_settled(&self) -> bool {
                true
            }
            async fn send_prepared(&self, _: ()) -> Result<(), Self::Error> {
                let started = self.started.lock().unwrap().take().unwrap();
                let release = self.release.lock().unwrap().take().unwrap();
                let _ = started.send(());
                let _ = release.await;
                self.events.lock().unwrap().push("write");
                Ok(())
            }
            async fn close_shared_bounded(&self, _: Duration) -> std::io::Result<()> {
                self.events.lock().unwrap().push("close");
                Ok(())
            }
            fn abort(&self) {
                self.aborted
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            async fn close(self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let (_open_tx, open_rx) = mpsc::channel("ordered_shutdown.open", 1);
        let (_close_tx, close_rx) = mpsc::channel("ordered_shutdown.close", 1);
        let (control_tx, control_rx) = mpsc::unbounded_channel("ordered_shutdown.control");
        let (started_tx, started_rx) = oneshot::channel("ordered_shutdown.started");
        let (release_tx, release_rx) = oneshot::channel("ordered_shutdown.release");
        let events = Arc::new(Mutex::new(Vec::new()));
        let aborted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut connection = Connection::pre_handshake(
            GatedCloseTx {
                started: Mutex::new(Some(started_tx)),
                release: Mutex::new(Some(release_rx)),
                events: events.clone(),
                aborted: aborted.clone(),
            },
            PendingRx,
            None,
            open_rx,
            close_rx,
            control_tx.clone(),
            control_rx,
            None,
            None,
        );
        let lane = connection
            .establish_from_handshake(test_handshake(
                ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
                ConnectionSettings {
                    parity: Parity::Even,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
            ))
            .unwrap();
        let core = connection.connection_core.clone();
        let sender = lane.sender.clone();
        let send = tokio::spawn(async move {
            sender
                .send(ConnectionMessage::Channel(ChannelMessage {
                    id: vox_types::ChannelId(1),
                    body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                        metadata: Metadata::default(),
                    }),
                }))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        let mut run = tokio::spawn(async move { connection.run().await });
        control_tx.send(DropControlRequest::shutdown()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut run)
                .await
                .is_err(),
            "shutdown passed an unsettled admitted write"
        );
        assert!(events.lock().unwrap().is_empty());
        let _ = release_tx.send(());
        assert!(send.await.unwrap().is_ok());
        tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*events.lock().unwrap(), ["write", "close"]);
        assert!(core.inner.lock().unwrap().tx.is_none());
        assert!(!aborted.load(std::sync::atomic::Ordering::Acquire));
    }

    #[tokio::test]
    async fn shutdown_detaches_core_sender_when_service_lane_is_retained() {
        let (mut connection, _control) =
            make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let lanes = Arc::new(Mutex::new(Vec::new()));
        connection.lane_acceptor = Some(Arc::new(RetainLanes(lanes.clone())));
        connection
            .handle_inbound_open(LaneId(2), inbound_open(1, 1))
            .await;
        let core = {
            let guard = lanes.lock().unwrap();
            assert_eq!(guard.len(), 1);
            guard[0].sender.connection_core.clone()
        };
        connection
            .control_tx
            .send(DropControlRequest::shutdown())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), connection.run())
            .await
            .unwrap();
        drop(connection);
        assert!(
            core.inner.lock().unwrap().tx.is_none(),
            "explicit shutdown retained the core sender behind a held service lane"
        );
        // CapturingTx does not support shared close or abort. This only
        // checks the core reference; it does not establish transport teardown.
        drop(lanes);
    }

    #[tokio::test]
    async fn cancelling_shutdown_during_shared_close_requests_abort() {
        struct StalledCloseTx {
            closing: Mutex<Option<oneshot::Sender<()>>>,
            aborted: Arc<std::sync::atomic::AtomicBool>,
        }
        impl ConduitTx for StalledCloseTx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            type Prepared = ();
            fn prepare_send(&self, _: Message<'_>) -> Result<(), Self::Error> {
                Ok(())
            }
            async fn send_prepared(&self, _: ()) -> Result<(), Self::Error> {
                Ok(())
            }
            async fn close_shared_bounded(&self, _: Duration) -> std::io::Result<()> {
                if let Some(closing) = self.closing.lock().unwrap().take() {
                    let _ = closing.send(());
                }
                std::future::pending().await
            }
            fn abort(&self) {
                self.aborted
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            async fn close(self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (_open_tx, open_rx) = mpsc::channel("cancel_shutdown.open", 1);
        let (_close_tx, close_rx) = mpsc::channel("cancel_shutdown.close", 1);
        let (control_tx, control_rx) = mpsc::unbounded_channel("cancel_shutdown.control");
        let (closing_tx, closing_rx) = oneshot::channel("cancel_shutdown.started");
        let aborted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut connection = Connection::pre_handshake(
            StalledCloseTx {
                closing: Mutex::new(Some(closing_tx)),
                aborted: aborted.clone(),
            },
            PendingRx,
            None,
            open_rx,
            close_rx,
            control_tx,
            control_rx,
            None,
            None,
        );
        let core = connection.connection_core.clone();
        let task = tokio::spawn(async move {
            connection
                .shutdown_outbound_bounded(Instant::now() + Duration::from_secs(5))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), closing_rx)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(aborted.load(std::sync::atomic::Ordering::Acquire));
        assert!(core.inner.lock().unwrap().tx.is_none());
        // The abort request and detached core sender do not prove that an
        // independently owned transport writer has stopped.
    }

    #[tokio::test]
    async fn dropping_unpolled_connection_run_requests_abort() {
        struct AbortTrackingTx(Arc<std::sync::atomic::AtomicBool>);
        impl ConduitTx for AbortTrackingTx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            type Prepared = ();
            fn prepare_send(&self, _: Message<'_>) -> Result<(), Self::Error> {
                Ok(())
            }
            async fn send_prepared(&self, _: ()) -> Result<(), Self::Error> {
                Ok(())
            }
            fn abort(&self) {
                self.0.store(true, std::sync::atomic::Ordering::Release);
            }
            async fn close(self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (_open_tx, open_rx) = mpsc::channel("unpolled_shutdown.open", 1);
        let (_close_tx, close_rx) = mpsc::channel("unpolled_shutdown.close", 1);
        let (control_tx, control_rx) = mpsc::unbounded_channel("unpolled_shutdown.control");
        let aborted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut connection = Connection::pre_handshake(
            AbortTrackingTx(aborted.clone()),
            PendingRx,
            None,
            open_rx,
            close_rx,
            control_tx,
            control_rx,
            None,
            None,
        );
        let core = connection.connection_core.clone();
        let task = tokio::spawn(async move { connection.run().await });
        // This current-thread test does not yield between spawn and abort, so
        // run's first poll and the shutdown-scoped guard have not happened.
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(aborted.load(std::sync::atomic::Ordering::Acquire));
        assert!(core.admission_is_stopped());
        assert!(core.inner.lock().unwrap().tx.is_none());
        // Abort can be unsupported or asynchronous; this test proves only
        // that it was requested and the core reference was detached.
    }

    #[test]
    fn connection_drop_does_not_wait_for_busy_sender_lock() {
        let (connection, _lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let held = core.inner.lock().unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            drop(connection);
            let _ = done_tx.send(());
        });
        let timely = done_rx.recv_timeout(Duration::from_millis(250));
        assert!(held.tx.is_some());
        drop(held);
        thread.join().unwrap();
        assert!(timely.is_ok(), "connection drop waited on the sender lock");
        assert!(core.admission_is_stopped());
        // The busy lock prevented detachment and the transport outcome remains
        // unknown; nonblocking cleanup is not proof of teardown.
        assert!(core.inner.lock().unwrap().tx.is_some());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn connection_drop_wakes_registered_admission_waiter() {
        let (connection, _lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let waiting_core = core.clone();
        let (started_tx, started_rx) = oneshot::channel("owner_drop.waiter_started");
        let mut waiter = tokio::spawn(async move {
            let stopped = waiting_core.wait_admission_stopped().fuse();
            let yielded = tokio::task::yield_now().fuse();
            futures_util::pin_mut!(stopped, yielded);
            futures_util::select_biased! {
                () = stopped => panic!("admission was already stopped"),
                () = yielded => {}
            }
            let _ = started_tx.send(());
            stopped.await;
        });
        started_rx.await.unwrap();
        drop(connection);
        let completed = tokio::time::timeout(Duration::from_millis(200), &mut waiter).await;
        if completed.is_err() {
            waiter.abort();
            let _ = waiter.await;
        }
        assert!(completed.is_ok(), "connection drop stranded the waiter");
        tokio::time::timeout(Duration::from_millis(200), core.wait_admission_stopped())
            .await
            .expect("late waiter did not observe connection owner drop");
    }

    #[tokio::test]
    async fn expired_shutdown_deadline_does_not_start_shared_close() {
        let (mut connection, _lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let error = connection
            .shutdown_outbound_bounded(Instant::now())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(core.inner.lock().unwrap().tx.is_none());
        // CapturingTx has no supported abort; detached is not torn down.
    }

    #[tokio::test]
    #[ignore = "diagnostic: retained sender may use a closed and recreated lane ID"]
    async fn retained_sender_cannot_send_after_lane_id_recreation() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (mut connection, _control) = make_capturing_connection(sent.clone());
        let settings = ConnectionSettings {
            parity: Parity::Odd,
            max_concurrent_requests: 1,
            initial_channel_credit: 1,
        };
        let old_lane =
            connection.make_connection_handle(LaneId(2), settings.clone(), settings.clone());
        let stale_sender = old_lane.sender.clone();
        connection.remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local);
        let _new_lane = connection.make_connection_handle(LaneId(2), settings.clone(), settings);
        let result = stale_sender
            .send(ConnectionMessage::Channel(vox_types::ChannelMessage {
                id: vox_types::ChannelId(1),
                body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                    metadata: Metadata::default(),
                }),
            }))
            .await;
        let frames = sent.lock().unwrap().len();
        connection.outbound_abort.abort();
        assert!(
            result.is_err(),
            "sender from the old lane accepted a frame on the new lane"
        );
        assert_eq!(
            frames, 0,
            "old sender emitted traffic after its lane closed"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lane_retirement_before_commit_rejects_pending_batch() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (mut connection, _control) = make_capturing_connection(sent.clone());
        let core = connection.connection_core.clone();
        let settings = ConnectionSettings {
            parity: Parity::Odd,
            max_concurrent_requests: 1,
            initial_channel_credit: 1,
        };
        let old = connection.make_connection_handle(LaneId(2), settings.clone(), settings.clone());
        let sender = old.sender.clone();
        let (started_tx, started_rx) = oneshot::channel("lane_commit_race.started");
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *core.before_admission_commit.lock().unwrap() = Some(Box::new(move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
        }));
        let send = tokio::spawn(async move {
            sender
                .send(ConnectionMessage::Channel(vox_types::ChannelMessage {
                    id: vox_types::ChannelId(3),
                    body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                        metadata: Metadata::default(),
                    }),
                }))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        let _ = connection.remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local);
        let _replacement = connection.make_connection_handle(LaneId(2), settings.clone(), settings);
        release_tx.send(()).unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(2), send)
            .await
            .unwrap()
            .unwrap();
        let frames = sent.lock().unwrap().len();
        connection.outbound_abort.abort();
        assert!(outcome.is_err());
        assert_eq!(frames, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retiring_an_inflight_lane_preserves_its_id_and_fails_closed() {
        let (mut connection, _control) =
            make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let settings = ConnectionSettings {
            parity: Parity::Odd,
            max_concurrent_requests: 1,
            initial_channel_credit: 1,
        };
        let old = connection.make_connection_handle(LaneId(2), settings.clone(), settings);
        let generation = old.sender.lane_generation();
        let lease = generation.acquire_write().ok().unwrap();
        assert!(
            connection
                .remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local)
                .is_some()
        );
        assert!(connection.resource_exhausted);
        assert!(connection.connection_core.admission_is_stopped());
        assert!(
            matches!(connection.conns.get(&LaneId(2)), Some(ConnectionSlot::Retiring(held)) if Arc::ptr_eq(held, &generation))
        );
        connection.handle_inbound_accept(LaneId(2), accept_ref());
        connection.handle_inbound_reject(LaneId(2), reject_ref());
        assert!(
            matches!(connection.conns.get(&LaneId(2)), Some(ConnectionSlot::Retiring(held)) if Arc::ptr_eq(held, &generation))
        );
        assert!(
            connection
                .remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local)
                .is_none()
        );
        assert!(connection.conns.contains_key(&LaneId(2)));
        lease.settle(true).unwrap();
        assert!(connection.conns.contains_key(&LaneId(2)));
    }

    #[tokio::test]
    async fn recreated_lane_gets_fresh_request_and_schema_send_state() {
        let (mut connection, _control) =
            make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let settings = ConnectionSettings {
            parity: Parity::Odd,
            max_concurrent_requests: 1,
            initial_channel_credit: 1,
        };
        let _old = connection.make_connection_handle(LaneId(2), settings.clone(), settings.clone());
        let core = connection.connection_core.clone();
        core.record_incoming_call(LaneId(2), RequestId(7), vox_types::MethodId(11));
        let old_state = core
            .inner
            .lock()
            .unwrap()
            .conns
            .get(&LaneId(2))
            .unwrap()
            .clone();
        assert!(
            old_state
                .lock()
                .unwrap()
                .inflight_incoming
                .contains_key(&RequestId(7))
        );
        connection.remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local);
        let _new = connection.make_connection_handle(LaneId(2), settings.clone(), settings);
        let new_state = core
            .inner
            .lock()
            .unwrap()
            .conns
            .get(&LaneId(2))
            .unwrap()
            .clone();
        assert!(!Arc::ptr_eq(&old_state, &new_state));
        assert!(new_state.lock().unwrap().inflight_incoming.is_empty());
        old_state
            .lock()
            .unwrap()
            .inflight_incoming
            .insert(RequestId(9), vox_types::MethodId(13));
        assert!(new_state.lock().unwrap().inflight_incoming.is_empty());
        connection.outbound_abort.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn busy_lane_retirement_does_not_block_or_reuse_id() {
        let (mut connection, _control) =
            make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let settings = ConnectionSettings {
            parity: Parity::Odd,
            max_concurrent_requests: 1,
            initial_channel_credit: 1,
        };
        let old = connection.make_connection_handle(LaneId(2), settings.clone(), settings);
        let generation = old.sender.lane_generation();
        let held = generation.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            held.with_lock_for_test(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let removing = std::thread::spawn(move || {
            let removed = connection
                .remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local)
                .is_some();
            result_tx.send(removed).unwrap();
            connection
        });
        let timely = result_rx.recv_timeout(Duration::from_secs(1));
        release_tx.send(()).unwrap();
        thread.join().unwrap();
        let connection = removing.join().unwrap();
        assert!(timely.expect("retirement blocked on a held lane lock"));
        assert!(connection.resource_exhausted);
        assert!(
            matches!(connection.conns.get(&LaneId(2)), Some(ConnectionSlot::Retiring(held)) if Arc::ptr_eq(held, &generation))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn settled_transport_rechecks_lane_at_write_lease_acquisition() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (mut connection, _control) = make_settled_capturing_connection(sent.clone());
        let settings = ConnectionSettings {
            parity: Parity::Odd,
            max_concurrent_requests: 1,
            initial_channel_credit: 1,
        };
        let old = connection.make_connection_handle(LaneId(2), settings.clone(), settings.clone());
        let sender = old.sender.clone();
        let (started_tx, started_rx) = oneshot::channel("settled.lane_race.started");
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *connection
            .connection_core
            .before_worker_write
            .lock()
            .unwrap() = Some(Box::new(move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
        }));
        let send = tokio::spawn(async move {
            sender
                .send(ConnectionMessage::Channel(vox_types::ChannelMessage {
                    id: vox_types::ChannelId(3),
                    body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                        metadata: Metadata::default(),
                    }),
                }))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        connection.remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local);
        let _replacement = connection.make_connection_handle(LaneId(2), settings.clone(), settings);
        release_tx.send(()).unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(2), send)
            .await
            .unwrap()
            .unwrap();
        connection.outbound_abort.abort();
        assert!(outcome.is_err());
        assert!(sent.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queued_response_requires_matching_identity_and_settled_transport() {
        for (settled, wrong_method) in [(true, false), (false, false), (true, true)] {
            let sent = Arc::new(Mutex::new(Vec::new()));
            let (connection, lane) = if settled {
                make_settled_capturing_connection(sent.clone())
            } else {
                make_capturing_connection(sent.clone())
            };
            let core = connection.connection_core.clone();
            let request_id = RequestId(77);
            let method_id = vox_types::MethodId(9);
            let generation = lane.sender.lane_generation();
            let book = response_reservation::RequestBook::for_lane(generation.clone());
            let request = book.record(request_id, method_id).unwrap();
            let (_, reservation) = book.reserve(&request).ok().unwrap();
            core.record_incoming_call(
                lane.lane_id(),
                request_id,
                if wrong_method {
                    vox_types::MethodId(11)
                } else {
                    method_id
                },
            );
            let ret: Result<u32, vox_types::VoxError<core::convert::Infallible>> = Ok(7);
            let (mut batch, result, _channels, _gate) = core
                .prepare_outbound_batch(
                    Message {
                        lane_id: lane.lane_id(),
                        payload: MessagePayload::RequestMessage(RequestMessage {
                            id: request_id,
                            body: RequestBody::Response(RequestResponse {
                                metadata: Metadata::default(),
                                ret: vox_types::Payload::outgoing(&ret),
                                schemas: Default::default(),
                            }),
                        }),
                    },
                    None,
                    None,
                    None,
                    Vec::new(),
                    Some(lane.sender.lane_closed.clone()),
                    Some(generation),
                )
                .ok()
                .unwrap();
            reservation
                .commit_with(|queued| {
                    batch.queued_response = Some(queued);
                    assert!(
                        core.outbound_tx
                            .try_send(OutboundCommand::Batch(Box::new(batch)))
                            .is_ok()
                    );
                })
                .unwrap();
            let result = tokio::time::timeout(Duration::from_secs(2), result)
                .await
                .unwrap()
                .unwrap();
            if settled && !wrong_method {
                assert!(result.is_ok());
                assert!(sent.lock().unwrap().iter().any(|msg| matches!(&msg.payload, CapturedPayload::Response { request_id: id, .. } if *id == request_id)));
                assert!(book.record(request_id, method_id).is_ok());
            } else {
                assert!(result.is_err());
                assert!(sent.lock().unwrap().is_empty());
                assert!(matches!(
                    book.reserve(&request),
                    Err(response_reservation::ReservationError::AlreadyReserved)
                ));
            }
            connection.outbound_abort.abort();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reserved_reply_uses_identity_through_real_queue_admission() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (connection, lane) = make_settled_capturing_connection(sent.clone());
        let request_id = RequestId(77);
        let method_id = vox_types::MethodId(9);
        let book = response_reservation::RequestBook::for_lane(lane.sender.lane_generation());
        let request = book.record(request_id, method_id).unwrap();
        let (_, reservation) = book.reserve(&request).ok().unwrap();
        lane.sender
            .connection_core
            .record_incoming_call(lane.lane_id(), request_id, method_id);
        let ret: Result<u32, vox_types::VoxError<core::convert::Infallible>> = Ok(7);
        let result = lane
            .sender
            .send_reserved_response(
                request_id,
                RequestResponse {
                    metadata: Metadata::default(),
                    ret: vox_types::Payload::outgoing(&ret),
                    schemas: Default::default(),
                },
                reservation,
            )
            .await;
        assert!(result.is_ok());
        assert!(sent.lock().unwrap().iter().any(|msg| matches!(&msg.payload, CapturedPayload::Response { request_id: id, .. } if *id == request_id)));
        assert!(book.record(request_id, method_id).is_ok());
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn cancelling_reserved_reply_before_queue_releases_its_identity() {
        let (connection, lane) =
            make_settled_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let request_id = RequestId(77);
        let method_id = vox_types::MethodId(9);
        let book = response_reservation::RequestBook::for_lane(lane.sender.lane_generation());
        let request = book.record(request_id, method_id).unwrap();
        let (_, reservation) = book.reserve(&request).ok().unwrap();
        lane.sender
            .connection_core
            .record_incoming_call(lane.lane_id(), request_id, method_id);
        let ret: Result<u32, vox_types::VoxError<core::convert::Infallible>> = Ok(7);
        let admission = lane.sender.connection_core.admission.lock().await;
        let mut send = Box::pin(lane.sender.send_reserved_response(
            request_id,
            RequestResponse {
                metadata: Metadata::default(),
                ret: vox_types::Payload::outgoing(&ret),
                schemas: Default::default(),
            },
            reservation,
        ));
        assert!(matches!(
            futures_util::poll!(&mut send),
            std::task::Poll::Pending
        ));
        drop(send);
        drop(admission);
        assert!(book.reserve(&request).is_ok());
        connection.outbound_abort.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_worker_does_not_allow_lane_id_reuse_while_writer_continues() {
        struct BackgroundWriteTx {
            sent: Arc<Mutex<Vec<LaneId>>>,
            started: Mutex<Option<oneshot::Sender<()>>>,
            release: Mutex<Option<oneshot::Receiver<()>>>,
        }
        impl ConduitTx for BackgroundWriteTx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            type Prepared = LaneId;
            fn send_success_is_locally_settled(&self) -> bool {
                true
            }
            fn prepare_send(&self, item: Message<'_>) -> Result<LaneId, Self::Error> {
                Ok(item.lane_id)
            }
            async fn send_prepared(&self, lane: LaneId) -> Result<(), Self::Error> {
                let started = self.started.lock().unwrap().take();
                let release = self.release.lock().unwrap().take();
                let sent = self.sent.clone();
                tokio::spawn(async move {
                    if let Some(started) = started {
                        let _ = started.send(());
                    }
                    if let Some(release) = release {
                        let _ = release.await;
                    }
                    sent.lock().unwrap().push(lane);
                })
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))?;
                Ok(())
            }
            async fn close(self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = oneshot::channel("background.started");
        let (release_tx, release_rx) = oneshot::channel("background.release");
        let (_open_tx, open_rx) = mpsc::channel("background.open", 1);
        let (_close_tx, close_rx) = mpsc::channel("background.close", 1);
        let (control_tx, control_rx) = mpsc::unbounded_channel("background.control");
        let mut connection = Connection::pre_handshake(
            BackgroundWriteTx {
                sent: sent.clone(),
                started: Mutex::new(Some(started_tx)),
                release: Mutex::new(Some(release_rx)),
            },
            PendingRx,
            None,
            open_rx,
            close_rx,
            control_tx,
            control_rx,
            None,
            None,
        );
        let settings = ConnectionSettings {
            parity: Parity::Odd,
            max_concurrent_requests: 1,
            initial_channel_credit: 1,
        };
        let _control = connection
            .establish_from_handshake(test_handshake(settings.clone(), settings.clone()))
            .unwrap();
        let old = connection.make_connection_handle(LaneId(2), settings.clone(), settings.clone());
        let generation = old.sender.lane_generation();
        let sender = old.sender.clone();
        let send = tokio::spawn(async move {
            sender
                .send(ConnectionMessage::Channel(vox_types::ChannelMessage {
                    id: vox_types::ChannelId(3),
                    body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                        metadata: Metadata::default(),
                    }),
                }))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        connection.remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local);
        assert!(connection.resource_exhausted);
        assert!(
            matches!(connection.conns.get(&LaneId(2)), Some(ConnectionSlot::Retiring(held)) if Arc::ptr_eq(held, &generation))
        );
        let replacement =
            connection.try_make_connection_handle(LaneId(2), settings.clone(), settings);
        assert!(replacement.is_err());
        let result = tokio::time::timeout(Duration::from_secs(2), send)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while sent.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(*sent.lock().unwrap(), vec![LaneId(2)]);
        // The late write shows why abort is not a teardown or delivery proof.
        assert!(connection.conns.contains_key(&LaneId(2)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unconfirmed_success_quarantines_retirement_before_deferred_write() {
        struct DeferredTx {
            sent: Arc<Mutex<Vec<LaneId>>>,
            release: Mutex<Option<oneshot::Receiver<()>>>,
            done: Mutex<Option<oneshot::Sender<()>>>,
        }
        impl ConduitTx for DeferredTx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            type Prepared = LaneId;
            fn prepare_send(&self, item: Message<'_>) -> Result<LaneId, Self::Error> {
                Ok(item.lane_id)
            }
            async fn send_prepared(&self, lane: LaneId) -> Result<(), Self::Error> {
                let release = self.release.lock().unwrap().take();
                let done = self.done.lock().unwrap().take();
                let sent = self.sent.clone();
                let _background = tokio::spawn(async move {
                    if let Some(release) = release {
                        let _ = release.await;
                    }
                    sent.lock().unwrap().push(lane);
                    if let Some(done) = done {
                        let _ = done.send(());
                    }
                });
                Ok(())
            }
            async fn close(self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (release_tx, release_rx) = oneshot::channel("unconfirmed.release");
        let (done_tx, done_rx) = oneshot::channel("unconfirmed.done");
        let (_open_tx, open_rx) = mpsc::channel("unconfirmed.open", 1);
        let (_close_tx, close_rx) = mpsc::channel("unconfirmed.close", 1);
        let (control_tx, control_rx) = mpsc::unbounded_channel("unconfirmed.control");
        let mut connection = Connection::pre_handshake(
            DeferredTx {
                sent: sent.clone(),
                release: Mutex::new(Some(release_rx)),
                done: Mutex::new(Some(done_tx)),
            },
            PendingRx,
            None,
            open_rx,
            close_rx,
            control_tx,
            control_rx,
            None,
            None,
        );
        let settings = ConnectionSettings {
            parity: Parity::Odd,
            max_concurrent_requests: 1,
            initial_channel_credit: 1,
        };
        let _control = connection
            .establish_from_handshake(test_handshake(settings.clone(), settings.clone()))
            .unwrap();
        let old = connection.make_connection_handle(LaneId(2), settings.clone(), settings.clone());
        old.sender
            .send(ConnectionMessage::Channel(vox_types::ChannelMessage {
                id: vox_types::ChannelId(3),
                body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                    metadata: Metadata::default(),
                }),
            }))
            .await
            .unwrap();
        assert!(sent.lock().unwrap().is_empty());
        connection.remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local);
        assert!(connection.resource_exhausted);
        assert!(matches!(
            connection.conns.get(&LaneId(2)),
            Some(ConnectionSlot::Retiring(_))
        ));
        assert!(
            connection
                .try_make_connection_handle(LaneId(2), settings.clone(), settings)
                .is_err()
        );
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), done_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*sent.lock().unwrap(), vec![LaneId(2)]);
        // Successful enqueue and abort alone did not stop the background write.
        assert!(connection.conns.contains_key(&LaneId(2)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lane_close_between_worker_check_and_write_is_rejected() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (mut connection, _control) = make_capturing_connection(sent.clone());
        let core = connection.connection_core.clone();
        let settings = ConnectionSettings {
            parity: Parity::Odd,
            max_concurrent_requests: 1,
            initial_channel_credit: 1,
        };
        let old = connection.make_connection_handle(LaneId(2), settings.clone(), settings.clone());
        let sender = old.sender.clone();
        let (started_tx, started_rx) = oneshot::channel("lane_write_race.started");
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *core.before_worker_write.lock().unwrap() = Some(Box::new(move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
        }));
        let send = tokio::spawn(async move {
            sender
                .send(ConnectionMessage::Channel(vox_types::ChannelMessage {
                    id: vox_types::ChannelId(3),
                    body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                        metadata: Metadata::default(),
                    }),
                }))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        let _ = connection.remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local);
        let _replacement = connection.make_connection_handle(LaneId(2), settings.clone(), settings);
        release_tx.send(()).unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(2), send)
            .await
            .unwrap()
            .unwrap();
        let frames = sent.lock().unwrap().len();
        connection.outbound_abort.abort();
        assert!(
            outcome.is_err() && frames == 0,
            "prechecked old lane wrote after retirement: outcome={outcome:?}, frames={frames}"
        );
    }

    #[tokio::test]
    #[ignore = "diagnostic: queued old-lane frames can outlive lane retirement"]
    async fn queued_old_lane_frame_cannot_write_after_lane_recreation() {
        struct DelayedFirstTx {
            sent: Arc<Mutex<Vec<LaneId>>>,
            started: Mutex<Option<oneshot::Sender<()>>>,
            release: Mutex<Option<oneshot::Receiver<()>>>,
        }
        impl ConduitTx for DelayedFirstTx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            type Prepared = LaneId;
            fn prepare_send(&self, item: Message<'_>) -> Result<LaneId, Self::Error> {
                Ok(item.lane_id)
            }
            async fn send_prepared(&self, lane_id: LaneId) -> Result<(), Self::Error> {
                let release = self.release.lock().unwrap().take();
                if let Some(release) = release {
                    if let Some(started) = self.started.lock().unwrap().take() {
                        let _ = started.send(());
                    }
                    let _ = release.await;
                }
                self.sent.lock().unwrap().push(lane_id);
                Ok(())
            }
            async fn close(self) -> std::io::Result<()> {
                Ok(())
            }
        }
        fn channel() -> vox_types::ChannelMessage<'static> {
            vox_types::ChannelMessage {
                id: vox_types::ChannelId(9),
                body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                    metadata: Metadata::default(),
                }),
            }
        }
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (_open_tx, open_rx) = mpsc::channel("old_lane.open", 1);
        let (_close_tx, close_rx) = mpsc::channel("old_lane.close", 1);
        let (control_tx, control_rx) = mpsc::unbounded_channel("old_lane.control");
        let (started_tx, started_rx) = oneshot::channel("old_lane.started");
        let (release_tx, release_rx) = oneshot::channel("old_lane.release");
        let mut connection = Connection::pre_handshake(
            DelayedFirstTx {
                sent: sent.clone(),
                started: Mutex::new(Some(started_tx)),
                release: Mutex::new(Some(release_rx)),
            },
            PendingRx,
            None,
            open_rx,
            close_rx,
            control_tx,
            control_rx,
            None,
            None,
        );
        let settings = ConnectionSettings {
            parity: Parity::Odd,
            max_concurrent_requests: 1,
            initial_channel_credit: 1,
        };
        let control = connection
            .establish_from_handshake(test_handshake(settings.clone(), settings.clone()))
            .unwrap();
        let old = connection.make_connection_handle(LaneId(2), settings.clone(), settings.clone());
        let first_sender = control.sender.clone();
        let first = tokio::spawn(async move {
            first_sender
                .send(ConnectionMessage::Channel(channel()))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        old.sender
            .try_send_channel_with_writer_schema(channel(), None)
            .unwrap();
        let _ = connection.remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local);
        let _new = connection.make_connection_handle(LaneId(2), settings.clone(), settings);
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), first)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(2),
            control.sender.send(ConnectionMessage::Channel(channel())),
        )
        .await
        .unwrap()
        .unwrap();
        let observed = sent.lock().unwrap().clone();
        connection.outbound_abort.abort();
        assert!(
            !observed.contains(&LaneId(2)),
            "retired lane was written by queued work: {observed:?}"
        );
    }

    #[tokio::test]
    #[ignore = "diagnostic: channel gate lookup does not include lane lifetime"]
    async fn recreated_lane_does_not_reuse_old_channel_gate() {
        let (mut connection, _control) =
            make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let settings = ConnectionSettings {
            parity: Parity::Odd,
            max_concurrent_requests: 1,
            initial_channel_credit: 1,
        };
        let old = connection.make_connection_handle(LaneId(2), settings.clone(), settings.clone());
        let old_gate = core.register_channel_gate(
            LaneId(2),
            &old.sender.lane_generation(),
            vox_types::ChannelId(3),
        );
        let _ = connection.remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local);
        let new = connection.make_connection_handle(LaneId(2), settings.clone(), settings);
        let new_gate = core.register_channel_gate(
            LaneId(2),
            &new.sender.lane_generation(),
            vox_types::ChannelId(3),
        );
        old_gate.finish(GateState::Failed);
        let new_state = new_gate.state();
        connection.outbound_abort.abort();
        assert!(
            !Arc::ptr_eq(&old_gate, &new_gate),
            "new lane inherited old lane's gate"
        );
        assert_eq!(new_state, GateState::Pending);
    }

    #[tokio::test]
    async fn lane_close_wakes_old_gate_waiters_without_opening_replacement() {
        fn channel() -> ConnectionMessage<'static> {
            ConnectionMessage::Channel(vox_types::ChannelMessage {
                id: vox_types::ChannelId(3),
                body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                    metadata: Metadata::default(),
                }),
            })
        }
        let (mut connection, _control) =
            make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let settings = ConnectionSettings {
            parity: Parity::Odd,
            max_concurrent_requests: 1,
            initial_channel_credit: 1,
        };
        let old = connection.make_connection_handle(LaneId(2), settings.clone(), settings.clone());
        let old_gate = core.register_channel_gate(
            LaneId(2),
            &old.sender.lane_generation(),
            vox_types::ChannelId(3),
        );
        let first_sender = old.sender.clone();
        let second_sender = old.sender.clone();
        let mut first = tokio::spawn(async move { first_sender.send(channel()).await });
        let mut second = tokio::spawn(async move { second_sender.send(channel()).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut first)
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut second)
                .await
                .is_err()
        );
        let _ = connection.remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local);
        for waiter in [first, second] {
            assert!(
                tokio::time::timeout(Duration::from_secs(2), waiter)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err()
            );
        }
        assert_eq!(old_gate.state(), GateState::Failed);
        let replacement = connection.make_connection_handle(LaneId(2), settings.clone(), settings);
        let generation = replacement.sender.lane_generation();
        let replacement_gate =
            core.register_channel_gate(LaneId(2), &generation, vox_types::ChannelId(3));
        assert_eq!(replacement_gate.state(), GateState::Pending);
        let replacement_sender = replacement.sender.clone();
        let mut waiting = tokio::spawn(async move { replacement_sender.send(channel()).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting)
                .await
                .is_err()
        );
        core.open_channel_gates(LaneId(2), &generation, &[vox_types::ChannelId(3)]);
        assert!(
            tokio::time::timeout(Duration::from_secs(2), waiting)
                .await
                .unwrap()
                .unwrap()
                .is_ok()
        );
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn retained_lane_cannot_send_after_explicit_shutdown() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (mut connection, _control) = make_capturing_connection(sent.clone());
        let lanes = Arc::new(Mutex::new(Vec::new()));
        connection.lane_acceptor = Some(Arc::new(RetainLanes(lanes.clone())));
        connection
            .handle_inbound_open(LaneId(2), inbound_open(1, 1))
            .await;
        let sender = {
            let held = lanes.lock().unwrap();
            assert_eq!(held.len(), 1);
            held[0].sender.clone()
        };
        connection
            .control_tx
            .send(DropControlRequest::shutdown())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), connection.run())
            .await
            .unwrap();
        drop(connection);
        let count_before = sent.lock().unwrap().len();
        let message = ConnectionMessage::Channel(vox_types::ChannelMessage {
            id: vox_types::ChannelId(1),
            body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                metadata: Metadata::default(),
            }),
        });
        let outcome = tokio::time::timeout(Duration::from_secs(2), sender.send(message))
            .await
            .unwrap();
        outcome.expect_err("retained lane accepted an outbound message after shutdown");
        let late = vox_types::ChannelMessage {
            id: vox_types::ChannelId(1),
            body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                metadata: Metadata::default(),
            }),
        };
        assert!(matches!(
            sender.try_send_channel_with_writer_schema(late, None),
            Err(TrySendError::Closed(()))
        ));
        assert_eq!(sent.lock().unwrap().len(), count_before);
    }

    #[tokio::test]
    async fn binder_panic_during_real_encoding_fails_registered_gate() {
        #[derive(facet::Facet)]
        struct Args {
            input: vox_types::Rx<u32>,
        }
        type Captured = (
            vox_types::ChannelId,
            Arc<dyn vox_types::ChannelSink>,
            Arc<ChannelDeclarationGate>,
        );
        struct PanicBinder<'a> {
            inner: &'a dyn vox_types::ChannelBinder,
            core: Arc<ConnectionCore>,
            lane_id: LaneId,
            generation: Arc<LaneLifetime>,
            captured: Arc<Mutex<Option<Captured>>>,
        }
        impl vox_types::ChannelBinder for PanicBinder<'_> {
            fn create_tx(&self) -> (vox_types::ChannelId, Arc<dyn vox_types::ChannelSink>) {
                let (id, sink) = self.inner.create_tx();
                let gate = self
                    .core
                    .channel_gates
                    .lock()
                    .unwrap()
                    .get(&(self.lane_id, Arc::as_ptr(&self.generation) as usize, id))
                    .and_then(std::sync::Weak::upgrade)
                    .expect("registered gate");
                gate.poison_for_test();
                *self.captured.lock().unwrap() = Some((id, sink, gate));
                panic!("injected binder panic after channel allocation");
            }
            fn create_rx(&self) -> (vox_types::ChannelId, vox_types::BoundChannelReceiver) {
                self.inner.create_rx()
            }
            fn bind_tx(&self, id: vox_types::ChannelId) -> Arc<dyn vox_types::ChannelSink> {
                self.inner.bind_tx(id)
            }
            fn register_rx(&self, id: vox_types::ChannelId) -> vox_types::BoundChannelReceiver {
                self.inner.register_rx(id)
            }
        }
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (connection, lane) = make_capturing_connection(sent.clone());
        let sender = lane.sender.clone();
        let core = connection.connection_core.clone();
        let driver = crate::Driver::new(lane, ());
        let inner = driver.caller();
        let captured: Arc<Mutex<Option<Captured>>> = Arc::new(Mutex::new(None));
        let binder = PanicBinder {
            inner: &inner,
            core: core.clone(),
            lane_id: sender.lane_id(),
            generation: sender.lane_generation(),
            captured: captured.clone(),
        };
        let (_paired_tx, input) = vox_types::channel::<u32>();
        let args = Args { input };
        let request = ConnectionMessage::Request(RequestMessage {
            id: RequestId(77),
            body: RequestBody::Call(vox_types::RequestCall {
                method_id: vox_types::MethodId(1),
                channels: Vec::new(),
                args: vox_types::Payload::outgoing(&args),
                schemas: Default::default(),
                metadata: Metadata::default(),
            }),
        });
        let result = std::panic::AssertUnwindSafe(sender.send_with_binder(request, Some(&binder)))
            .catch_unwind()
            .await;
        assert!(result.is_err());
        let (id, sink, gate) = captured
            .lock()
            .unwrap()
            .take()
            .expect("binder allocated a channel");
        assert_eq!(gate.state(), GateState::Failed);
        assert!(sink.is_closed());
        tokio::time::timeout(Duration::from_millis(200), sink.closed())
            .await
            .unwrap();
        let outcome = sender
            .send(ConnectionMessage::Channel(vox_types::ChannelMessage {
                id,
                body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                    metadata: Metadata::default(),
                }),
            }))
            .await;
        assert!(outcome.is_err());
        assert!(sent.lock().unwrap().is_empty());
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn reentrant_capture_cleanup_fails_closed_without_double_panic() {
        let (connection, _lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let generation = Arc::new(LaneLifetime::new());
        let capture = DeclarationCapture::begin(&core, LaneId(1), Some(&generation));
        let gate = core.register_channel_gate(LaneId(1), &generation, vox_types::ChannelId(3));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            DECLARATION_CAPTURES.with(|captures| {
                let _borrowed = captures.borrow_mut();
                drop(capture);
                panic!("injected outer unwind after reentrant cleanup");
            });
        }));
        assert!(result.is_err());
        assert!(core.admission_is_stopped());
        assert_eq!(gate.state(), GateState::Failed);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), gate.wait())
                .await
                .unwrap()
                .is_err()
        );
        let next = DeclarationCapture::begin(&core, LaneId(1), Some(&generation));
        assert!(
            DECLARATION_CAPTURES
                .with(|captures| captures.borrow().iter().all(|frame| frame.active.get()))
        );
        drop(next);
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn encoder_panic_fails_only_its_own_captured_gates() {
        let (connection, _lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let generation = Arc::new(LaneLifetime::new());
        let other_generation = Arc::new(LaneLifetime::new());
        let outer = DeclarationCapture::begin(&core, LaneId(1), Some(&generation));
        let outer_gate =
            core.register_channel_gate(LaneId(1), &generation, vox_types::ChannelId(3));
        let other_lane_gate =
            core.register_channel_gate(LaneId(3), &other_generation, vox_types::ChannelId(3));
        let inner_gate = Arc::new(Mutex::new(None));
        let saved = inner_gate.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _inner = DeclarationCapture::begin(&core, LaneId(1), Some(&generation));
            let gate = core.register_channel_gate(LaneId(1), &generation, vox_types::ChannelId(5));
            *saved.lock().unwrap() = Some(gate);
            panic!("injected encoder panic after channel registration");
        }));
        assert!(result.is_err());
        assert_eq!(
            inner_gate.lock().unwrap().as_ref().unwrap().state(),
            GateState::Failed
        );
        assert_eq!(outer_gate.state(), GateState::Pending);
        outer.finish().open();
        assert_eq!(outer_gate.state(), GateState::Open);
        assert_eq!(other_lane_gate.state(), GateState::Pending);
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn channel_gates_for_equal_ids_on_distinct_lanes_are_independent() {
        let (connection, _lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let channel_id = vox_types::ChannelId(3);
        // Each lane's Driver has its own allocator, so both can allocate this ID.
        let first_lane = LaneId(1);
        let second_lane = LaneId(3);
        let first_generation = Arc::new(LaneLifetime::new());
        let second_generation = Arc::new(LaneLifetime::new());
        let _first_gate = core.register_channel_gate(first_lane, &first_generation, channel_id);
        let _second_gate = core.register_channel_gate(second_lane, &second_generation, channel_id);
        let first_core = core.clone();
        let first_wait_generation = first_generation.clone();
        let first = tokio::spawn(async move {
            first_core
                .await_channel_gate(first_lane, Some(&first_wait_generation), channel_id)
                .await
                .unwrap();
        });
        let second_core = core.clone();
        let second_wait_generation = second_generation.clone();
        let mut second = tokio::spawn(async move {
            second_core
                .await_channel_gate(second_lane, Some(&second_wait_generation), channel_id)
                .await
                .unwrap();
        });
        core.open_channel_gates(first_lane, &first_generation, &[channel_id]);
        tokio::time::timeout(Duration::from_secs(2), first)
            .await
            .unwrap()
            .unwrap();
        assert!(core.channel_gate_open(first_lane, &first_generation, channel_id));
        assert!(
            !core.channel_gate_open(second_lane, &second_generation, channel_id),
            "opening one lane must not open another lane's gate"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut second)
                .await
                .is_err()
        );
        core.open_channel_gates(second_lane, &second_generation, &[channel_id]);
        tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .unwrap()
            .unwrap();
        assert!(core.channel_gate_open(second_lane, &second_generation, channel_id));
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn failed_admission_notification_wakes_parked_waiter() {
        let (connection, _lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let mut waiter = Box::pin(core.wait_admission_stopped());
        assert!(matches!(
            futures_util::poll!(&mut waiter),
            std::task::Poll::Pending
        ));
        core.injected_admission_notification_failures
            .store(1, std::sync::atomic::Ordering::Release);
        core.force_stop_admission();
        assert!(core.admission_is_stopped());
        let outcome = tokio::time::timeout(Duration::from_millis(50), &mut waiter).await;
        if outcome.is_err() {
            // This cleanup wake is not counted as success.
            core.admission_stopped_tx.send(true).unwrap();
            tokio::time::timeout(Duration::from_secs(2), waiter)
                .await
                .unwrap();
        }
        connection.outbound_abort.abort();
        assert!(
            outcome.is_ok(),
            "parked admission waiter was not woken after notification failure"
        );
    }

    #[tokio::test]
    async fn control_pump_signals_terminal_request_without_treating_lane_close_as_shutdown() {
        let (connection, _lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let (source_tx, source_rx) = mpsc::unbounded_channel("test.control.pump.source");
        let (destination_tx, mut destination_rx) =
            mpsc::unbounded_channel("test.control.pump.destination");
        let pump = tokio::spawn(run_control_pump(
            source_rx,
            destination_tx,
            Arc::downgrade(&core),
        ));
        source_tx
            .send(DropControlRequest::Close(LaneId(2)))
            .unwrap();
        assert!(matches!(
            destination_rx.recv().await,
            Some(DropControlRequest::Close(LaneId(2)))
        ));
        assert!(!core.admission_is_stopped());
        let request = DropControlRequest::shutdown();
        let DropControlRequest::Shutdown { deadline } = request else {
            unreachable!()
        };
        source_tx
            .send(DropControlRequest::Shutdown { deadline })
            .unwrap();
        let Some(DropControlRequest::Shutdown {
            deadline: forwarded,
        }) = destination_rx.recv().await
        else {
            panic!("shutdown request was not forwarded")
        };
        assert_eq!(forwarded, deadline, "pump reset the shutdown deadline");
        assert!(core.admission_is_stopped());
        drop(source_tx);
        tokio::time::timeout(Duration::from_secs(2), pump)
            .await
            .unwrap()
            .unwrap();
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn failed_capture_cleanup_wakes_admission_waiter() {
        let (connection, _lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let mut waiter = Box::pin(core.wait_admission_stopped());
        assert!(matches!(
            futures_util::poll!(&mut waiter),
            std::task::Poll::Pending
        ));
        core.injected_admission_notification_failures
            .store(1, std::sync::atomic::Ordering::Release);
        DeclarationCapture::stop_admission(&core);
        tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap();
        assert!(core.admission_is_stopped());
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn shutdown_wakes_producer_waiting_for_channel_gate() {
        let (mut connection, lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let sender = lane.sender.clone();
        let channel_id = vox_types::ChannelId(3);
        let _gate = connection.connection_core.register_channel_gate(
            sender.lane_id(),
            &sender.lane_generation(),
            channel_id,
        );
        let message = ConnectionMessage::Channel(vox_types::ChannelMessage {
            id: channel_id,
            body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                metadata: Metadata::default(),
            }),
        });
        let mut blocked = tokio::spawn(async move { sender.send(message).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut blocked)
                .await
                .is_err()
        );
        connection
            .control_tx
            .send(DropControlRequest::shutdown())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), connection.run())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), blocked)
            .await
            .unwrap()
            .unwrap()
            .expect_err("shutdown allowed a gate-blocked send to succeed");
    }

    #[tokio::test]
    async fn admission_cutoff_rejects_a_reserved_but_not_admitted_batch() {
        let (connection, lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let sender = lane.sender.clone();
        let core = connection.connection_core.clone();
        let guard = core.admission.lock().await;
        let mut stop = Box::pin(core.stop_outbound_admission());
        assert!(matches!(
            futures_util::poll!(&mut stop),
            std::task::Poll::Pending
        ));
        let message = ConnectionMessage::Channel(vox_types::ChannelMessage {
            id: vox_types::ChannelId(1),
            body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                metadata: Metadata::default(),
            }),
        });
        let mut send = Box::pin(sender.send(message));
        assert!(matches!(
            futures_util::poll!(&mut send),
            std::task::Poll::Pending
        ));
        drop(guard);
        stop.await;
        send.await
            .expect_err("a batch reserved before cutoff but not admitted must fail");
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn admission_cutoff_preserves_batch_admitted_before_it() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (connection, lane) = make_capturing_connection(sent.clone());
        let sender = lane.sender.clone();
        let core = connection.connection_core.clone();
        let guard = core.admission.lock().await;
        let message = ConnectionMessage::Channel(vox_types::ChannelMessage {
            id: vox_types::ChannelId(1),
            body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                metadata: Metadata::default(),
            }),
        });
        let mut send = Box::pin(sender.send(message));
        assert!(matches!(
            futures_util::poll!(&mut send),
            std::task::Poll::Pending
        ));
        let mut stop = Box::pin(core.stop_outbound_admission());
        assert!(matches!(
            futures_util::poll!(&mut stop),
            std::task::Poll::Pending
        ));
        drop(guard);
        let (sent_result, ()) = tokio::join!(send, stop);
        sent_result
            .expect("batch admitted before cutoff must complete when the transport succeeds");
        assert_eq!(sent.lock().unwrap().len(), 1);
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn admission_timeout_rejects_waiters_without_waiting_for_the_lock() {
        let (mut connection, lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let sender = lane.sender.clone();
        let core = connection.connection_core.clone();
        let held = core.admission.lock().await;
        let message = ConnectionMessage::Channel(vox_types::ChannelMessage {
            id: vox_types::ChannelId(1),
            body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                metadata: Metadata::default(),
            }),
        });
        let mut send = Box::pin(sender.send(message));
        assert!(matches!(
            futures_util::poll!(&mut send),
            std::task::Poll::Pending
        ));
        connection
            .control_tx
            .send(DropControlRequest::shutdown())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), connection.run())
            .await
            .unwrap();
        assert!(core.admission_is_stopped());
        tokio::time::timeout(Duration::from_secs(1), &mut send)
            .await
            .unwrap()
            .expect_err("waiter must fail without acquiring the held admission lock");
        drop(held);
    }

    #[tokio::test]
    async fn observed_eof_stops_outbound_admission() {
        struct EofRx;
        impl ConduitRx for EofRx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            async fn recv(&mut self) -> Result<Option<SelfRef<Message<'static>>>, Self::Error> {
                Ok(None)
            }
        }
        let (_open_tx, open_rx) = mpsc::channel("eof_admission.open", 1);
        let (_close_tx, close_rx) = mpsc::channel("eof_admission.close", 1);
        let (control_tx, control_rx) = mpsc::unbounded_channel("eof_admission.control");
        let mut connection = Connection::pre_handshake(
            CapturingTx {
                sent: Arc::new(Mutex::new(Vec::new())),
            },
            EofRx,
            None,
            open_rx,
            close_rx,
            control_tx,
            control_rx,
            None,
            None,
        );
        let lane = connection
            .establish_from_handshake(test_handshake(
                ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
                ConnectionSettings {
                    parity: Parity::Even,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
            ))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), connection.run())
            .await
            .unwrap();
        let message = ConnectionMessage::Channel(vox_types::ChannelMessage {
            id: vox_types::ChannelId(1),
            body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                metadata: Metadata::default(),
            }),
        });
        lane.sender
            .send(message)
            .await
            .expect_err("observed EOF must reject new sends");
        connection.outbound_abort.abort();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn forced_stop_may_overlap_a_preadmitted_async_enqueue() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (connection, lane) = make_capturing_connection(sent.clone());
        let core = connection.connection_core.clone();
        let (started, started_rx) = oneshot::channel("forced_async.started");
        let (release, wait) = std::sync::mpsc::channel::<()>();
        *core.before_admission_commit.lock().unwrap() = Some(Box::new(move || {
            let _ = started.send(());
            let _ = wait.recv();
        }));
        let sender = lane.sender.clone();
        let task = tokio::spawn(async move {
            sender
                .send(ConnectionMessage::Channel(vox_types::ChannelMessage {
                    id: vox_types::ChannelId(1),
                    body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                        metadata: Metadata::default(),
                    }),
                }))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        core.force_stop_admission();
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            sent.lock().unwrap().len(),
            1,
            "the preadmitted batch can complete after forced stop"
        );
        connection.outbound_abort.abort();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn forced_stop_may_overlap_a_preadmitted_try_enqueue() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (connection, lane) = make_capturing_connection(sent.clone());
        let core = connection.connection_core.clone();
        let (started, started_rx) = oneshot::channel("forced_try.started");
        let (release, wait) = std::sync::mpsc::channel::<()>();
        *core.before_admission_commit.lock().unwrap() = Some(Box::new(move || {
            let _ = started.send(());
            let _ = wait.recv();
        }));
        let sender = lane.sender.clone();
        let task = tokio::task::spawn_blocking(move || {
            sender.try_send_channel_with_writer_schema(
                vox_types::ChannelMessage {
                    id: vox_types::ChannelId(1),
                    body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                        metadata: Metadata::default(),
                    }),
                },
                None,
            )
        });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        core.force_stop_admission();
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while sent.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    #[ignore = "diagnostic: cancelled declaring calls can strand channel gates and prepared request state"]
    async fn cancelled_declaring_call_releases_waiters_and_prepared_state() {
        #[derive(facet::Facet)]
        struct Args {
            input: vox_types::Rx<u32>,
        }
        let (connection, lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let sender = lane.sender.clone();
        let core = connection.connection_core.clone();
        let driver = crate::Driver::new(lane, ());
        let binder = driver.caller();
        let (_paired_tx, input) = vox_types::channel::<u32>();
        let held_admission = core.admission.lock().await;
        let (declared, declared_rx) = oneshot::channel("cancel_declared.channels");
        let call_sender = sender.clone();
        let call = tokio::spawn(async move {
            let args = Args { input };
            let message = ConnectionMessage::Request(RequestMessage {
                id: RequestId(77),
                body: RequestBody::Call(vox_types::RequestCall {
                    method_id: vox_types::MethodId(1),
                    channels: Vec::new(),
                    args: vox_types::Payload::outgoing(&args),
                    schemas: Default::default(),
                    metadata: Metadata::default(),
                }),
            });
            call_sender
                .send_with_binder_and_method_observing_channels(
                    message,
                    Some(&binder),
                    None,
                    move |ids| {
                        let _ = declared.send(ids.to_vec());
                    },
                )
                .await
        });
        let ids = tokio::time::timeout(Duration::from_secs(2), declared_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ids.len(), 1);
        let spawn_waiter = |sender: ConnectionSender| {
            let id = ids[0];
            tokio::spawn(async move {
                sender
                    .send(ConnectionMessage::Channel(vox_types::ChannelMessage {
                        id,
                        body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                            metadata: Metadata::default(),
                        }),
                    }))
                    .await
            })
        };
        let mut first = spawn_waiter(sender.clone());
        let mut second = spawn_waiter(sender);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut first)
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut second)
                .await
                .is_err()
        );
        call.abort();
        let _cancelled = call.await.unwrap_err();
        drop(held_admission);
        let first_result = tokio::time::timeout(Duration::from_millis(50), &mut first).await;
        let second_result = tokio::time::timeout(Duration::from_millis(50), &mut second).await;
        let first_stranded = first_result.is_err();
        let second_stranded = second_result.is_err();
        let state = core
            .inner
            .lock()
            .unwrap()
            .conns
            .get(&LaneId::CONTROL)
            .unwrap()
            .clone();
        let stale_request = state
            .lock()
            .unwrap()
            .inflight_outgoing
            .contains_key(&RequestId(77));
        core.force_stop_admission();
        if first_stranded {
            let _ = tokio::time::timeout(Duration::from_secs(2), first)
                .await
                .unwrap();
        }
        if second_stranded {
            let _ = tokio::time::timeout(Duration::from_secs(2), second)
                .await
                .unwrap();
        }
        connection.outbound_abort.abort();
        assert!(
            !first_stranded && !second_stranded && !stale_request,
            "cancelled call left waiters stranded: {first_stranded}/{second_stranded}, stale request: {stale_request}"
        );
        assert!(matches!(first_result, Ok(Ok(Err(())))));
        assert!(matches!(second_result, Ok(Ok(Err(())))));
    }

    #[tokio::test]
    async fn prepared_calls_become_inflight_only_when_worker_accepts_them() {
        let (connection, lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let request_id = RequestId(77);
        let method_id = vox_types::MethodId(9);
        let arg = 7u32;
        let (batch, result, gates, gate_guard) = core
            .prepare_outbound_batch(
                Message {
                    lane_id: lane.sender.lane_id(),
                    payload: MessagePayload::RequestMessage(RequestMessage {
                        id: request_id,
                        body: RequestBody::Call(vox_types::RequestCall {
                            method_id,
                            channels: Vec::new(),
                            args: vox_types::Payload::outgoing(&arg),
                            schemas: Default::default(),
                            metadata: Metadata::default(),
                        }),
                    }),
                },
                None,
                None,
                None,
                Vec::new(),
                None,
                None,
            )
            .ok()
            .expect("prepare call");
        assert!(gates.is_empty());
        assert_eq!(
            core.take_outgoing_call_method(lane.sender.lane_id(), request_id),
            None
        );
        assert!(
            core.outbound_tx
                .send(OutboundCommand::Batch(Box::new(batch)))
                .await
                .is_ok()
        );
        gate_guard.open();
        result.await.unwrap().unwrap();
        assert_eq!(
            core.take_outgoing_call_method(lane.sender.lane_id(), request_id),
            Some(method_id)
        );
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn outbound_fence_waits_for_earlier_batch_and_fails_on_worker_error() {
        struct GatedTx {
            started: Mutex<Option<oneshot::Sender<()>>>,
            release: Mutex<Option<oneshot::Receiver<()>>>,
            fail: bool,
        }
        impl ConduitTx for GatedTx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            type Prepared = ();
            fn prepare_send(&self, _: Message<'_>) -> Result<(), Self::Error> {
                Ok(())
            }
            async fn send_prepared(&self, _: ()) -> Result<(), Self::Error> {
                let started = self.started.lock().unwrap().take().unwrap();
                let release = self.release.lock().unwrap().take().unwrap();
                let _ = started.send(());
                let _ = release.await;
                if self.fail {
                    Err(std::io::Error::other("injected write failure"))
                } else {
                    Ok(())
                }
            }
            async fn close(self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for fail in [false, true] {
            let (_open_tx, open_rx) = mpsc::channel("fence.open", 1);
            let (_close_tx, close_rx) = mpsc::channel("fence.close", 1);
            let (control_tx, control_rx) = mpsc::unbounded_channel("fence.control");
            let (started_tx, started_rx) = oneshot::channel("fence.started");
            let (release_tx, release_rx) = oneshot::channel("fence.release");
            let mut connection = Connection::pre_handshake(
                GatedTx {
                    started: Mutex::new(Some(started_tx)),
                    release: Mutex::new(Some(release_rx)),
                    fail,
                },
                PendingRx,
                None,
                open_rx,
                close_rx,
                control_tx,
                control_rx,
                None,
                None,
            );
            let lane = connection
                .establish_from_handshake(test_handshake(
                    ConnectionSettings {
                        parity: Parity::Odd,
                        max_concurrent_requests: 1,
                        initial_channel_credit: 1,
                    },
                    ConnectionSettings {
                        parity: Parity::Even,
                        max_concurrent_requests: 1,
                        initial_channel_credit: 1,
                    },
                ))
                .unwrap();
            let sender = lane.sender.clone();
            let send = tokio::spawn(async move {
                sender
                    .send(ConnectionMessage::Channel(ChannelMessage {
                        id: vox_types::ChannelId(1),
                        body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                            metadata: Metadata::default(),
                        }),
                    }))
                    .await
            });
            tokio::time::timeout(Duration::from_secs(2), started_rx)
                .await
                .unwrap()
                .unwrap();
            let (reached_tx, mut reached_rx) = oneshot::channel("fence.reached");
            assert!(
                connection
                    .connection_core
                    .outbound_tx
                    .send(OutboundCommand::Fence(reached_tx))
                    .await
                    .is_ok()
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut reached_rx)
                    .await
                    .is_err(),
                "fence passed a stalled batch"
            );
            let _ = release_tx.send(());
            let sent = tokio::time::timeout(Duration::from_secs(2), send)
                .await
                .unwrap()
                .unwrap();
            let reached = tokio::time::timeout(Duration::from_secs(2), reached_rx)
                .await
                .unwrap();
            assert_eq!(sent.is_err(), fail);
            if fail {
                assert!(!matches!(reached, Ok(Ok(()))));
            } else {
                assert!(matches!(reached, Ok(Ok(()))));
            }
            connection.outbound_abort.abort();
        }
    }

    #[tokio::test]
    async fn stop_and_fence_requires_the_admission_mutex_even_after_signal() {
        let (connection, _lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let held = core.admission.lock().await;
        core.force_stop_admission();
        let result = core
            .stop_and_fence(Instant::now() + Duration::from_millis(50))
            .await;
        assert_eq!(result, Err(FenceError::Deadline));
        assert!(core.admission_is_stopped());
        drop(held);
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn stop_and_fence_completes_when_worker_is_idle() {
        let (connection, _lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        assert_eq!(
            core.stop_and_fence(Instant::now() + Duration::from_secs(2))
                .await,
            Ok(())
        );
        assert!(core.admission_is_stopped());
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn outbound_fence_reports_an_earlier_rejected_batch() {
        let (mut connection, lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let (batch, result, _channels, _gate) = core
            .prepare_outbound_batch(
                Message {
                    lane_id: lane.lane_id(),
                    payload: MessagePayload::ChannelMessage(ChannelMessage {
                        id: vox_types::ChannelId(1),
                        body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                            metadata: Metadata::default(),
                        }),
                    }),
                },
                None,
                None,
                None,
                Vec::new(),
                Some(lane.sender.lane_closed.clone()),
                Some(lane.sender.lane_generation.clone()),
            )
            .ok()
            .unwrap();
        connection.close_all_connections(ConnectionCloseReason::Local);
        let (reached_tx, reached_rx) = oneshot::channel("fence.rejected");
        assert!(
            core.outbound_tx
                .send(OutboundCommand::Batch(Box::new(batch)))
                .await
                .is_ok()
        );
        assert!(
            core.outbound_tx
                .send(OutboundCommand::Fence(reached_tx))
                .await
                .is_ok()
        );
        assert!(result.await.unwrap().is_err());
        assert!(reached_rx.await.unwrap().is_err());
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn failed_response_write_stops_later_queued_frames() {
        struct ResponseFailTx {
            observed: Arc<Mutex<Vec<&'static str>>>,
            begun: Mutex<Option<oneshot::Sender<()>>>,
            release: Mutex<Option<oneshot::Receiver<()>>>,
        }
        impl ConduitTx for ResponseFailTx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            type Prepared = &'static str;
            fn prepare_send(&self, item: Message<'_>) -> Result<Self::Prepared, Self::Error> {
                Ok(match &item.payload {
                    MessagePayload::SchemaMessage(_) => "schema",
                    MessagePayload::RequestMessage(RequestMessage {
                        body: RequestBody::Response(_),
                        ..
                    }) => "response",
                    _ => "other",
                })
            }
            async fn send_prepared(&self, item: Self::Prepared) -> Result<(), Self::Error> {
                self.observed.lock().unwrap().push(item);
                if item == "response" {
                    let begun = self.begun.lock().unwrap().take();
                    let release = self.release.lock().unwrap().take();
                    if let Some(begun) = begun {
                        let _ = begun.send(());
                    }
                    if let Some(release) = release {
                        let _ = release.await;
                    }
                    return Err(std::io::Error::other("injected uncertain response write"));
                }
                Ok(())
            }
            async fn close(self) -> std::io::Result<()> {
                Ok(())
            }
        }
        fn channel() -> vox_types::ChannelMessage<'static> {
            vox_types::ChannelMessage {
                id: vox_types::ChannelId(99),
                body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                    metadata: Metadata::default(),
                }),
            }
        }
        let observed = Arc::new(Mutex::new(Vec::new()));
        let (mut connection, lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let sender = lane.sender.clone();
        let core = connection.connection_core.clone();
        let (begun_tx, begun_rx) = oneshot::channel("response_failure.begun");
        let (release_tx, release_rx) = oneshot::channel("response_failure.release");
        core.inner.lock().unwrap().tx = Some(Arc::new(ResponseFailTx {
            observed: observed.clone(),
            begun: Mutex::new(Some(begun_tx)),
            release: Mutex::new(Some(release_rx)),
        }));
        let request_id = RequestId(77);
        let method_id = vox_types::MethodId(9);
        core.record_incoming_call(sender.lane_id(), request_id, method_id);
        let response_sender = sender.clone();
        let response = tokio::spawn(async move {
            let ret: Result<u32, vox_types::VoxError<core::convert::Infallible>> = Ok(7);
            response_sender
                .send_response(
                    request_id,
                    RequestResponse {
                        ret: vox_types::Payload::outgoing(&ret),
                        metadata: Metadata::default(),
                        schemas: Default::default(),
                    },
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), begun_rx)
            .await
            .unwrap()
            .unwrap();
        sender
            .try_send_channel_with_writer_schema(channel(), None)
            .unwrap();
        release_tx.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), response)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        tokio::time::timeout(Duration::from_secs(2), core.outbound_tx.closed())
            .await
            .unwrap();
        assert!(matches!(
            sender.try_send_channel_with_writer_schema(channel(), None),
            Err(TrySendError::Closed(()))
        ));
        assert_eq!(*observed.lock().unwrap(), vec!["schema", "response"]);
        tokio::time::timeout(Duration::from_secs(2), connection.run())
            .await
            .unwrap();
        assert!(lane.close_reason().is_some());
        assert!(core.inner.lock().unwrap().tx.is_none());
    }

    #[tokio::test]
    async fn preparing_response_schema_does_not_mark_or_cache_it_as_sent() {
        use facet::Facet;
        let (connection, lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let lane_id = lane.sender.lane_id();
        let method_id = vox_types::MethodId(301);
        let error_method_id = vox_types::MethodId(303);
        let request_id = RequestId(77);
        let result: Result<u32, vox_types::VoxError<core::convert::Infallible>> = Ok(7);
        let mut ordinary = RequestResponse {
            ret: vox_types::Payload::outgoing(&result),
            metadata: Metadata::default(),
            schemas: Default::default(),
        };
        core.prepare_response_for_method(lane_id, request_id, method_id, &mut ordinary);
        assert!(!ordinary.schemas.is_empty());
        let mut error = RequestResponse {
            ret: vox_types::Payload::outgoing(&result),
            metadata: Metadata::default(),
            schemas: Default::default(),
        };
        core.prepare_response_for_shape(
            lane_id,
            request_id,
            error_method_id,
            <Result<u32, vox_types::VoxError<core::convert::Infallible>> as Facet>::SHAPE,
            &mut error,
        );
        assert!(!error.schemas.is_empty());
        let state = core
            .inner
            .lock()
            .unwrap()
            .conns
            .get(&lane_id)
            .unwrap()
            .clone();
        {
            let state = state.lock().unwrap();
            assert!(
                !state
                    .send_tracker
                    .has_sent_binding(method_id, vox_types::BindingDirection::Response)
            );
            assert!(
                !state
                    .send_tracker
                    .has_sent_binding(error_method_id, vox_types::BindingDirection::Response)
            );
            assert!(
                !state
                    .planned_bindings
                    .contains_key(&(vox_types::BindingDirection::Response, method_id))
            );
            assert!(
                !state
                    .planned_bindings
                    .contains_key(&(vox_types::BindingDirection::Response, error_method_id))
            );
        }
        core.record_incoming_call(lane_id, request_id, method_id);
        lane.sender
            .send_response(request_id, ordinary)
            .await
            .unwrap();
        assert!(
            state
                .lock()
                .unwrap()
                .send_tracker
                .has_sent_binding(method_id, vox_types::BindingDirection::Response)
        );
        assert!(
            !state
                .lock()
                .unwrap()
                .send_tracker
                .has_sent_binding(error_method_id, vox_types::BindingDirection::Response)
        );
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn prepared_response_keeps_binding_until_worker_accepts_it() {
        let (connection, lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let lane_id = lane.sender.lane_id();
        let request_id = RequestId(77);
        let method_id = vox_types::MethodId(9);
        core.record_incoming_call(lane_id, request_id, method_id);
        let ret: Result<u32, vox_types::VoxError<core::convert::Infallible>> = Ok(7);
        let prepare = || {
            core.prepare_outbound_batch(
                Message {
                    lane_id,
                    payload: MessagePayload::RequestMessage(RequestMessage {
                        id: request_id,
                        body: RequestBody::Response(RequestResponse {
                            metadata: Metadata::default(),
                            ret: vox_types::Payload::outgoing(&ret),
                            schemas: Default::default(),
                        }),
                    }),
                },
                None,
                None,
                None,
                Vec::new(),
                None,
                None,
            )
            .ok()
            .expect("prepare response")
        };
        let state = core
            .inner
            .lock()
            .unwrap()
            .conns
            .get(&lane_id)
            .unwrap()
            .clone();
        let abandoned = prepare();
        drop(abandoned);
        assert_eq!(
            state.lock().unwrap().inflight_incoming.get(&request_id),
            Some(&method_id)
        );
        let (batch, result, gates, gate_guard) = prepare();
        assert!(gates.is_empty());
        assert_eq!(
            state.lock().unwrap().inflight_incoming.get(&request_id),
            Some(&method_id)
        );
        assert!(
            core.outbound_tx
                .send(OutboundCommand::Batch(Box::new(batch)))
                .await
                .is_ok()
        );
        gate_guard.open();
        result.await.unwrap().unwrap();
        assert!(
            !state
                .lock()
                .unwrap()
                .inflight_incoming
                .contains_key(&request_id)
        );
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    #[ignore = "diagnostic: failed call preparation can release undeclared channels"]
    async fn preparation_failure_does_not_release_undeclared_channel() {
        #[derive(facet::Facet)]
        struct Args {
            input: vox_types::Rx<u32>,
        }
        struct RejectCallTx(CapturingTx);
        impl ConduitTx for RejectCallTx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            type Prepared = CapturedMessage;
            fn prepare_send(&self, item: Message<'_>) -> Result<Self::Prepared, Self::Error> {
                if matches!(
                    &item.payload,
                    MessagePayload::RequestMessage(RequestMessage {
                        body: RequestBody::Call(_),
                        ..
                    })
                ) {
                    return Err(std::io::Error::other("injected call preparation failure"));
                }
                self.0.prepare_send(item)
            }
            async fn send_prepared(&self, item: Self::Prepared) -> Result<(), Self::Error> {
                self.0.send_prepared(item).await
            }
            async fn close(self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (connection, lane) = make_capturing_connection(sent.clone());
        let sender = lane.sender.clone();
        let core = connection.connection_core.clone();
        core.inner.lock().unwrap().tx =
            Some(Arc::new(RejectCallTx(CapturingTx { sent: sent.clone() })));
        let driver = crate::Driver::new(lane, ());
        let binder = driver.caller();
        let (paired_tx, input) = vox_types::channel::<u32>();
        let args = Args { input };
        let mut declared = Vec::new();
        let result = sender
            .send_with_binder_and_method_observing_channels(
                ConnectionMessage::Request(RequestMessage {
                    id: RequestId(77),
                    body: RequestBody::Call(vox_types::RequestCall {
                        method_id: vox_types::MethodId(1),
                        channels: Vec::new(),
                        args: vox_types::Payload::outgoing(&args),
                        schemas: Default::default(),
                        metadata: Metadata::default(),
                    }),
                }),
                Some(&binder),
                None,
                |ids| declared.extend_from_slice(ids),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(declared.len(), 1);
        assert!(
            paired_tx.is_closed(),
            "failed declaration must close its retained sender"
        );
        tokio::time::timeout(Duration::from_millis(200), paired_tx.closed())
            .await
            .unwrap();
        assert!(matches!(
            paired_tx.try_send(8),
            Err(TrySendError::Closed(8))
        ));
        let channel = ConnectionMessage::Channel(vox_types::ChannelMessage {
            id: declared[0],
            body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                metadata: Metadata::default(),
            }),
        });
        let outcome = tokio::time::timeout(Duration::from_millis(200), sender.send(channel)).await;
        let unexpected_frames = sent.lock().unwrap().len();
        core.force_stop_admission();
        connection.outbound_abort.abort();
        assert!(
            matches!(outcome, Ok(Err(()))),
            "channel must fail, without remaining blocked or reaching the transport; outcome: {outcome:?}, frames: {unexpected_frames}"
        );
        assert_eq!(unexpected_frames, 0, "undeclared channel sent a frame");
    }

    #[tokio::test]
    #[ignore = "diagnostic: queue-full can open an undeclared channel and retain prepared state"]
    async fn full_queue_does_not_open_channels_from_rejected_call() {
        #[derive(facet::Facet)]
        struct Args {
            input: vox_types::Rx<u32>,
        }
        struct StalledTx(Mutex<Option<oneshot::Sender<()>>>);
        impl ConduitTx for StalledTx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            type Prepared = ();
            fn prepare_send(&self, _: Message<'_>) -> Result<(), Self::Error> {
                Ok(())
            }
            async fn send_prepared(&self, _: ()) -> Result<(), Self::Error> {
                if let Some(started) = self.0.lock().unwrap().take() {
                    let _ = started.send(());
                }
                std::future::pending().await
            }
            async fn close(self) -> std::io::Result<()> {
                Ok(())
            }
        }
        fn channel() -> vox_types::ChannelMessage<'static> {
            vox_types::ChannelMessage {
                id: vox_types::ChannelId(99),
                body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                    metadata: Metadata::default(),
                }),
            }
        }
        let (_open_tx, open_rx) = mpsc::channel("full_call.open", 1);
        let (_close_tx, close_rx) = mpsc::channel("full_call.close", 1);
        let (control_tx, control_rx) = mpsc::unbounded_channel("full_call.control");
        let (started_tx, started_rx) = oneshot::channel("full_call.started");
        let mut connection = Connection::pre_handshake(
            StalledTx(Mutex::new(Some(started_tx))),
            PendingRx,
            None,
            open_rx,
            close_rx,
            control_tx,
            control_rx,
            None,
            None,
        );
        let lane = connection
            .establish_from_handshake(test_handshake(
                ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
                ConnectionSettings {
                    parity: Parity::Even,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
            ))
            .unwrap();
        let sender = lane.sender.clone();
        let core = connection.connection_core.clone();
        let driver = crate::Driver::new(lane, ());
        let binder = driver.caller();
        let first_sender = sender.clone();
        let first = tokio::spawn(async move {
            first_sender
                .send(ConnectionMessage::Channel(channel()))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        let capacity = core.outbound_queue_stats().1;
        for _ in 0..capacity {
            sender
                .try_send_channel_with_writer_schema(channel(), None)
                .unwrap();
        }
        assert_eq!(core.outbound_queue_stats(), (capacity, capacity));
        let (_paired_tx, input) = vox_types::channel::<u32>();
        let args = Args { input };
        let declared = Arc::new(Mutex::new(Vec::new()));
        let declared_out = declared.clone();
        let observed_core = core.clone();
        *core.before_admission_commit.lock().unwrap() = Some(Box::new(move || {
            *declared_out.lock().unwrap() = observed_core
                .channel_gates
                .lock()
                .unwrap()
                .keys()
                .copied()
                .collect();
        }));
        let rejected = core.try_send_with_options(
            Message {
                lane_id: sender.lane_id(),
                payload: MessagePayload::RequestMessage(RequestMessage {
                    id: RequestId(77),
                    body: RequestBody::Call(vox_types::RequestCall {
                        method_id: vox_types::MethodId(1),
                        channels: Vec::new(),
                        args: vox_types::Payload::outgoing(&args),
                        schemas: Default::default(),
                        metadata: Metadata::default(),
                    }),
                }),
            },
            Some(&binder),
            None,
            None,
            Vec::new(),
            Some(sender.lane_closed.clone()),
            Some(sender.lane_generation()),
        );
        let registered = declared.lock().unwrap().clone();
        assert_eq!(
            registered.len(),
            1,
            "the call must register its channel gate"
        );
        assert_eq!(registered[0].0, sender.lane_id());
        let channel_open =
            core.channel_gate_open(registered[0].0, &sender.lane_generation(), registered[0].2);
        let state = core
            .inner
            .lock()
            .unwrap()
            .conns
            .get(&sender.lane_id())
            .unwrap()
            .clone();
        let stale_request = state
            .lock()
            .unwrap()
            .inflight_outgoing
            .contains_key(&RequestId(77));
        first.abort();
        let _ = first.await;
        connection.outbound_abort.abort();
        assert!(matches!(rejected, Err(TrySendError::Full(()))));
        assert!(
            !channel_open && !stale_request,
            "rejected call opened channel: {channel_open}, retained request: {stale_request}"
        );
    }

    #[tokio::test]
    async fn shutdown_wakes_producer_waiting_for_outbound_capacity() {
        struct StalledTx(Mutex<Option<oneshot::Sender<()>>>);
        impl ConduitTx for StalledTx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            type Prepared = ();
            fn prepare_send(&self, _: Message<'_>) -> Result<(), Self::Error> {
                Ok(())
            }
            async fn send_prepared(&self, _: ()) -> Result<(), Self::Error> {
                let started = self.0.lock().unwrap().take();
                if let Some(started) = started {
                    let _ = started.send(());
                }
                std::future::pending().await
            }
            async fn close(self) -> std::io::Result<()> {
                Ok(())
            }
        }
        fn channel() -> vox_types::ChannelMessage<'static> {
            vox_types::ChannelMessage {
                id: vox_types::ChannelId(1),
                body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                    metadata: Metadata::default(),
                }),
            }
        }
        let (_open_tx, open_rx) = mpsc::channel("shutdown_capacity.open", 1);
        let (_close_tx, close_rx) = mpsc::channel("shutdown_capacity.close", 1);
        let (control_tx, control_rx) = mpsc::unbounded_channel("shutdown_capacity.control");
        let (started, started_rx) = oneshot::channel("shutdown_capacity.started");
        let mut connection = Connection::pre_handshake(
            StalledTx(Mutex::new(Some(started))),
            PendingRx,
            None,
            open_rx,
            close_rx,
            control_tx.clone(),
            control_rx,
            None,
            None,
        );
        let lane = connection
            .establish_from_handshake(test_handshake(
                ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
                ConnectionSettings {
                    parity: Parity::Even,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
            ))
            .unwrap();
        let sender = lane.sender.clone();
        let first_sender = sender.clone();
        let first = tokio::spawn(async move {
            first_sender
                .send(ConnectionMessage::Channel(channel()))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        let capacity = connection.connection_core.outbound_queue_stats().1;
        for _ in 0..capacity {
            sender
                .try_send_channel_with_writer_schema(channel(), None)
                .unwrap();
        }
        assert_eq!(
            connection.connection_core.outbound_queue_stats(),
            (capacity, capacity)
        );
        let blocked_sender = sender.clone();
        let mut blocked = tokio::spawn(async move {
            blocked_sender
                .send(ConnectionMessage::Channel(channel()))
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut blocked)
                .await
                .is_err()
        );
        control_tx.send(DropControlRequest::shutdown()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), connection.run())
            .await
            .unwrap();
        let outcome = tokio::time::timeout(Duration::from_millis(200), &mut blocked).await;
        first.abort();
        blocked.abort();
        connection.outbound_abort.abort();
        let result = outcome
            .expect("shutdown left the capacity-blocked producer waiting")
            .unwrap();
        result.expect_err("shutdown allowed a capacity-blocked send to succeed");
    }

    #[tokio::test]
    async fn shutdown_interrupts_stalled_local_lane_open() {
        struct StalledTx(Mutex<Option<oneshot::Sender<()>>>);
        impl ConduitTx for StalledTx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            type Prepared = ();
            fn prepare_send(&self, _: Message<'_>) -> Result<(), Self::Error> {
                Ok(())
            }
            async fn send_prepared(&self, _: ()) -> Result<(), Self::Error> {
                if let Some(started) = self.0.lock().unwrap().take() {
                    let _ = started.send(());
                }
                std::future::pending().await
            }
            async fn close(self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let (open_tx, open_rx) = mpsc::channel("stalled_open.open", 1);
        let (_close_tx, close_rx) = mpsc::channel("stalled_open.close", 1);
        let (control_tx, control_rx) = mpsc::unbounded_channel("stalled_open.control");
        let (started_tx, started_rx) = oneshot::channel("stalled_open.started");
        let (result_tx, result_rx) = oneshot::channel("stalled_open.result");
        let mut connection = Connection::pre_handshake(
            StalledTx(Mutex::new(Some(started_tx))),
            PendingRx,
            None,
            open_rx,
            close_rx,
            control_tx.clone(),
            control_rx,
            None,
            None,
        );
        connection
            .establish_from_handshake(test_handshake(
                ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
                ConnectionSettings {
                    parity: Parity::Even,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
            ))
            .unwrap();
        open_tx
            .send(OpenRequest {
                settings: ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
                metadata: Metadata::default(),
                result_tx,
            })
            .await
            .unwrap();
        let worker_abort = connection.outbound_abort.clone();
        let mut run = tokio::spawn(async move { connection.run().await });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        control_tx.send(DropControlRequest::shutdown()).unwrap();
        let shutdown = tokio::time::timeout(Duration::from_millis(250), &mut run).await;
        if shutdown.is_err() {
            run.abort();
            let _ = run.await;
        }
        worker_abort.abort();
        assert!(
            shutdown.is_ok(),
            "shutdown did not interrupt the stalled open"
        );
        assert!(
            result_rx.await.is_err(),
            "interrupted open reported a result"
        );
    }

    #[tokio::test]
    async fn shutdown_interrupts_stalled_lane_rejection() {
        struct StalledTx(Mutex<Option<oneshot::Sender<()>>>);
        impl ConduitTx for StalledTx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            type Prepared = ();
            fn prepare_send(&self, _: Message<'_>) -> Result<(), Self::Error> {
                Ok(())
            }
            async fn send_prepared(&self, _: ()) -> Result<(), Self::Error> {
                if let Some(started) = self.0.lock().unwrap().take() {
                    let _ = started.send(());
                }
                std::future::pending().await
            }
            async fn close(self) -> std::io::Result<()> {
                Ok(())
            }
        }
        struct OpenRx(Option<SelfRef<Message<'static>>>);
        impl ConduitRx for OpenRx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            async fn recv(&mut self) -> Result<Option<SelfRef<Message<'static>>>, Self::Error> {
                if let Some(message) = self.0.take() {
                    Ok(Some(message))
                } else {
                    std::future::pending().await
                }
            }
        }
        let (_open_tx, open_rx) = mpsc::channel("stalled_shutdown.open", 1);
        let (_close_tx, close_rx) = mpsc::channel("stalled_shutdown.close", 1);
        let (control_tx, control_rx) = mpsc::unbounded_channel("stalled_shutdown.control");
        let (started_tx, started_rx) = oneshot::channel("stalled_shutdown.started");
        let message = Message {
            lane_id: LaneId(2),
            payload: MessagePayload::LaneOpen(LaneOpen {
                connection_settings: ConnectionSettings {
                    parity: Parity::Even,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
                metadata: Metadata::default(),
            }),
        };
        let mut connection = Connection::pre_handshake(
            StalledTx(Mutex::new(Some(started_tx))),
            OpenRx(Some(SelfRef::owning(
                Backing::Boxed(Box::default()),
                message,
            ))),
            None,
            open_rx,
            close_rx,
            control_tx.clone(),
            control_rx,
            None,
            None,
        );
        connection
            .establish_from_handshake(test_handshake(
                ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
                ConnectionSettings {
                    parity: Parity::Even,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
            ))
            .unwrap();
        let worker_abort = connection.outbound_abort.clone();
        let mut run = tokio::spawn(async move { connection.run().await });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        control_tx.send(DropControlRequest::shutdown()).unwrap();
        let shutdown = tokio::time::timeout(Duration::from_millis(250), &mut run).await;
        if shutdown.is_err() {
            run.abort();
            let _ = run.await;
        }
        worker_abort.abort();
        assert!(
            shutdown.is_ok(),
            "shutdown was not observed while lane rejection was stalled"
        );
    }

    #[tokio::test]
    async fn rejected_lane_does_not_wait_forever_for_stalled_peer() {
        struct StalledTx(Option<oneshot::Sender<()>>);
        impl Drop for StalledTx {
            fn drop(&mut self) {
                if let Some(done) = self.0.take() {
                    let _ = done.send(());
                }
            }
        }
        impl ConduitTx for StalledTx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            type Prepared = ();
            fn prepare_send(&self, _: Message<'_>) -> Result<(), Self::Error> {
                Ok(())
            }
            async fn send_prepared(&self, _: ()) -> Result<(), Self::Error> {
                std::future::pending().await
            }
            async fn close(self) -> std::io::Result<()> {
                Ok(())
            }
        }
        struct OpenRx(Option<SelfRef<Message<'static>>>);
        impl ConduitRx for OpenRx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            async fn recv(&mut self) -> Result<Option<SelfRef<Message<'static>>>, Self::Error> {
                if let Some(message) = self.0.take() {
                    Ok(Some(message))
                } else {
                    std::future::pending().await
                }
            }
        }
        let (_open_tx, open_rx) = mpsc::channel("test.open.stalled", 1);
        let (_close_tx, close_rx) = mpsc::channel("test.close.stalled", 1);
        let (control_tx, control_rx) = mpsc::unbounded_channel("test.control.stalled");
        let (dropped_tx, dropped_rx) = oneshot::channel("test.stalled_tx.dropped");
        let message = Message {
            lane_id: LaneId(2),
            payload: MessagePayload::LaneOpen(LaneOpen {
                connection_settings: ConnectionSettings {
                    parity: Parity::Even,
                    max_concurrent_requests: 1,
                    initial_channel_credit: 1,
                },
                metadata: Metadata::default(),
            }),
        };
        let mut connection = Connection::pre_handshake(
            StalledTx(Some(dropped_tx)),
            OpenRx(Some(SelfRef::owning(
                Backing::Boxed(Box::default()),
                message,
            ))),
            None,
            open_rx,
            close_rx,
            control_tx,
            control_rx,
            None,
            None,
        );
        connection.resource_limits = Some(ConnectionResourceLimits {
            max_peer_lane_opens: 0,
            ..hardened_test_limits()
        });
        let retained_core = connection.connection_core.clone();
        tokio::time::timeout(Duration::from_secs(3), connection.run())
            .await
            .expect("connection must stop when its rejection cannot be sent");
        assert!(connection.resource_exhausted);
        tokio::time::timeout(Duration::from_secs(2), dropped_rx)
            .await
            .expect("blocked worker must release its conduit")
            .unwrap();
        assert!(retained_core.inner.lock().unwrap().tx.is_none());

        let (_open_tx, open_rx) = mpsc::channel("test.open.protocol_stalled", 1);
        let (_close_tx, close_rx) = mpsc::channel("test.close.protocol_stalled", 1);
        let (control_tx, control_rx) = mpsc::unbounded_channel("test.control.protocol_stalled");
        let (dropped_tx, dropped_rx) = oneshot::channel("test.protocol_stalled_tx.dropped");
        let mut connection = Connection::pre_handshake(
            StalledTx(Some(dropped_tx)),
            PendingRx,
            None,
            open_rx,
            close_rx,
            control_tx.clone(),
            control_rx,
            None,
            None,
        );
        connection.resource_limits = Some(hardened_test_limits());
        control_tx
            .send(DropControlRequest::ProtocolClose {
                conn_id: LaneId(2),
                description: "peer exceeded request limit".into(),
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), connection.run())
            .await
            .expect("protocol close must stop when the peer does not read");
        tokio::time::timeout(Duration::from_secs(2), dropped_rx)
            .await
            .expect("blocked protocol-error send must release its conduit")
            .unwrap();
    }

    #[tokio::test]
    async fn resource_deadline_closes_connection_stalled_on_full_lane_queue() {
        struct FloodRx(usize);
        impl ConduitRx for FloodRx {
            type Error = std::io::Error;
            type Msg = MessageFamily;
            async fn recv(&mut self) -> Result<Option<SelfRef<Message<'static>>>, Self::Error> {
                if self.0 == 0 {
                    return std::future::pending().await;
                }
                self.0 -= 1;
                Ok(Some(SelfRef::owning(
                    Backing::Boxed(Box::default()),
                    Message {
                        lane_id: LaneId::CONTROL,
                        payload: MessagePayload::ChannelMessage(vox_types::ChannelMessage {
                            id: vox_types::ChannelId(1),
                            body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                                metadata: Metadata::default(),
                            }),
                        }),
                    },
                )))
            }
        }
        let (_open_tx, open_rx) = mpsc::channel("test.open.queue_stalled", 1);
        let (_close_tx, close_rx) = mpsc::channel("test.close.queue_stalled", 1);
        let (control_tx, control_rx) = mpsc::unbounded_channel("test.control.queue_stalled");
        let mut connection = Connection::pre_handshake(
            CapturingTx {
                sent: Arc::new(Mutex::new(Vec::new())),
            },
            FloodRx(100),
            None,
            open_rx,
            close_rx,
            control_tx,
            control_rx,
            None,
            None,
        );
        connection.resource_limits = Some(ConnectionResourceLimits {
            max_message_processing_time: Duration::from_millis(25),
            ..hardened_test_limits()
        });
        let mut control = connection
            .establish_from_handshake(test_handshake(
                ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 64,
                    initial_channel_credit: 16,
                },
                ConnectionSettings {
                    parity: Parity::Even,
                    max_concurrent_requests: 64,
                    initial_channel_credit: 16,
                },
            ))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), connection.run())
            .await
            .expect("full lane queue must not stall hardened connection forever");
        assert!(connection.resource_exhausted);
        assert_eq!(control.close_reason(), Some(ConnectionCloseReason::Local));
        let mut buffered = 0;
        while control.rx.try_recv().is_ok() {
            buffered += 1;
        }
        assert_eq!(buffered, 64);
    }

    #[tokio::test]
    async fn resource_limits_bound_unregistered_channel_flood() {
        let (mut connection, mut control) =
            make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        connection.resource_limits = Some(hardened_test_limits());
        for id in [1, 1, 3, 5] {
            let message = Message {
                lane_id: LaneId::CONTROL,
                payload: MessagePayload::ChannelMessage(vox_types::ChannelMessage {
                    id: vox_types::ChannelId(id),
                    body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                        metadata: Metadata::default(),
                    }),
                }),
            };
            connection
                .handle_message(
                    SelfRef::owning(Backing::Boxed(Box::default()), message),
                    Default::default(),
                    &mut None,
                )
                .await;
        }
        assert!(connection.resource_exhausted);
        let Some(ConnectionSlot::Active(state)) = connection.conns.get(&LaneId::CONTROL) else {
            panic!("missing control lane")
        };
        assert_eq!(state.peer_channel_ids.len(), 2);
        for _ in 0..3 {
            assert!(control.rx.recv().await.is_some());
        }
        assert!(control.rx.try_recv().is_err());
        connection.close_all_connections(ConnectionCloseReason::Local);
        control.closed().await;
        assert_eq!(control.close_reason(), Some(ConnectionCloseReason::Local));
    }

    #[tokio::test]
    async fn oversized_single_grant_is_rejected_before_routing() {
        let (mut connection, mut control) =
            make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        connection.resource_limits = Some(hardened_test_limits());
        let message = Message {
            lane_id: LaneId::CONTROL,
            payload: MessagePayload::ChannelMessage(vox_types::ChannelMessage {
                id: vox_types::ChannelId(1),
                body: vox_types::ChannelBody::GrantCredit(vox_types::ChannelGrantCredit {
                    additional: u32::MAX,
                }),
            }),
        };
        connection
            .handle_message(
                SelfRef::owning(Backing::Boxed(Box::default()), message),
                Default::default(),
                &mut None,
            )
            .await;
        assert!(connection.resource_exhausted);
        assert!(control.rx.try_recv().is_err());
        let Some(ConnectionSlot::Active(state)) = connection.conns.get(&LaneId::CONTROL) else {
            panic!("missing control lane")
        };
        assert!(state.peer_channel_ids.is_empty());
    }

    #[tokio::test]
    async fn peer_call_lifetime_budget_bounds_request_bookkeeping() {
        let (mut connection, mut control) =
            make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        connection.resource_limits = Some(ConnectionResourceLimits {
            max_peer_calls_per_lane: 2,
            ..hardened_test_limits()
        });
        for id in [1, 3, 5] {
            let message = Message {
                lane_id: LaneId::CONTROL,
                payload: MessagePayload::RequestMessage(RequestMessage {
                    id: RequestId(id),
                    body: RequestBody::Call(RequestCall {
                        channels: Vec::new(),
                        method_id: vox_types::MethodId(id),
                        args: Payload::Encoded(&[]),
                        schemas: Default::default(),
                        metadata: Default::default(),
                    }),
                }),
            };
            connection
                .handle_message(
                    SelfRef::owning(Backing::Boxed(Box::default()), message),
                    Default::default(),
                    &mut None,
                )
                .await;
        }
        assert!(connection.resource_exhausted);
        let Some(ConnectionSlot::Active(state)) = connection.conns.get(&LaneId::CONTROL) else {
            panic!("missing control lane")
        };
        assert_eq!(state.peer_calls, 2);
        assert!(control.rx.recv().await.is_some());
        assert!(control.rx.recv().await.is_some());
        assert!(control.rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn proxied_request_ids_never_rebind_a_late_response() {
        let (connection, lane) =
            make_settled_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let mut requests = ProxiedRequests::new(Parity::Odd, 1);
        let source = RequestId(77);
        let method = vox_types::MethodId(9);
        let first = requests.begin(&lane.sender, source, method).unwrap();
        assert_eq!(first, RequestId(1));
        assert!(requests.begin(&lane.sender, source, method).is_err());
        assert!(requests.begin(&lane.sender, RequestId(79), method).is_err());
        let (actual_source, actual_method, capability) = requests.response(first).unwrap();
        assert_eq!((actual_source, actual_method), (source, method));
        lane.sender
            .response_book
            .retire_unreserved(&capability)
            .unwrap();
        requests.finish(first).unwrap();
        let second = requests.begin(&lane.sender, source, method).unwrap();
        assert_eq!(second, RequestId(3));
        assert!(requests.response(first).is_err());
        assert_eq!(requests.response(second).unwrap().0, source);
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn cancelled_proxy_requests_connection_abort_on_both_sides() {
        let (left_tx, mut left_rx) = mpsc::unbounded_channel("proxy_cancel.left");
        let (right_tx, mut right_rx) = mpsc::unbounded_channel("proxy_cancel.right");
        drop(ProxyTermination {
            left: Some(left_tx),
            right: Some(right_tx),
            left_lane: LaneId(2),
            right_lane: LaneId(3),
            uncertain: true,
        });
        assert!(matches!(
            left_rx.recv().await,
            Some(DropControlRequest::ResourceClose)
        ));
        assert!(matches!(
            right_rx.recv().await,
            Some(DropControlRequest::ResourceClose)
        ));
        // ResourceClose is a request; this does not prove transport teardown.
    }

    #[tokio::test]
    async fn duplicate_ingress_does_not_replace_live_request_binding() {
        let (mut connection, mut control) =
            make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        for method in [7, 9] {
            let message = Message {
                lane_id: LaneId::CONTROL,
                payload: MessagePayload::RequestMessage(RequestMessage {
                    id: RequestId(1),
                    body: RequestBody::Call(RequestCall {
                        channels: Vec::new(),
                        method_id: vox_types::MethodId(method),
                        args: Payload::Encoded(&[]),
                        schemas: vox_types::SchemaBytes(vec![method as u8]),
                        metadata: Default::default(),
                    }),
                }),
            };
            connection
                .handle_message(
                    SelfRef::owning(Backing::Boxed(Box::default()), message),
                    Default::default(),
                    &mut None,
                )
                .await;
        }
        let state = core
            .inner
            .lock()
            .unwrap()
            .conns
            .get(&LaneId::CONTROL)
            .unwrap()
            .clone();
        assert_eq!(
            state.lock().unwrap().inflight_incoming.get(&RequestId(1)),
            Some(&vox_types::MethodId(7)),
            "duplicate request replaced the original method binding"
        );
        let Some(ConnectionSlot::Active(lane)) = connection.conns.get(&LaneId::CONTROL) else {
            panic!("missing control lane before protocol-close processing")
        };
        assert_eq!(
            lane.schema_recv_tracker
                .writer_schema_bytes(vox_types::MethodId(7), BindingDirection::Args),
            Some(vec![7])
        );
        assert_eq!(
            lane.schema_recv_tracker
                .writer_schema_bytes(vox_types::MethodId(9), BindingDirection::Args),
            None,
            "duplicate request changed schema bookkeeping"
        );
        assert!(control.rx.try_recv().is_ok());
        assert!(control.rx.try_recv().is_err(), "duplicate was routed");
    }

    #[tokio::test]
    async fn schema_budget_counts_replacements_and_checks_before_retaining() {
        let (mut connection, _control) =
            make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        connection.resource_limits = Some(hardened_test_limits());
        for bytes in [vec![1, 2], vec![3, 4], vec![5]] {
            let message = Message {
                lane_id: LaneId::CONTROL,
                payload: MessagePayload::SchemaMessage(SchemaMessage {
                    method_id: vox_types::MethodId(1),
                    direction: BindingDirection::Args,
                    schemas: vox_types::SchemaBytes(bytes),
                }),
            };
            connection
                .handle_message(
                    SelfRef::owning(Backing::Boxed(Box::default()), message),
                    Default::default(),
                    &mut None,
                )
                .await;
        }
        assert!(connection.resource_exhausted);
        let Some(ConnectionSlot::Active(state)) = connection.conns.get(&LaneId::CONTROL) else {
            panic!("missing control lane")
        };
        assert_eq!(state.received_schema_count, 2);
        assert_eq!(state.received_schema_bytes, 4);
        assert_eq!(
            state
                .schema_recv_tracker
                .writer_schema_bytes(vox_types::MethodId(1), BindingDirection::Args),
            Some(vec![3, 4])
        );
    }

    // r[verify rpc.observability.connection-errors]
    #[test]
    fn connection_receive_errors_emit_diagnostics_and_non_graceful_close_reasons() {
        fn run_case(
            error: std::io::Error,
            expected_reason: ConnectionCloseReason,
            expect_decode_error: bool,
        ) {
            let driver_events = Arc::new(Mutex::new(Vec::new()));
            let transport_events = Arc::new(Mutex::new(Vec::new()));
            let observer: VoxObserverHandle = Arc::new(RecordingObserver {
                driver_events: driver_events.clone(),
                transport_events: transport_events.clone(),
            });

            let mut connection = make_connection_with_observer(observer);
            let handle = connection
                .establish_from_handshake(test_handshake(
                    ConnectionSettings {
                        parity: Parity::Odd,
                        max_concurrent_requests: 64,
                        initial_channel_credit: 16,
                    },
                    ConnectionSettings {
                        parity: Parity::Even,
                        max_concurrent_requests: 64,
                        initial_channel_credit: 16,
                    },
                ))
                .expect("establish observed connection");

            driver_events
                .lock()
                .expect("driver events mutex poisoned")
                .clear();
            transport_events
                .lock()
                .expect("transport events mutex poisoned")
                .clear();

            connection.observe_connection_recv_error(&error);
            connection.close_all_connections(classify_connection_recv_error(&error));

            assert_eq!(handle.close_reason(), Some(expected_reason));

            let driver_events = driver_events.lock().expect("driver events mutex poisoned");
            assert!(driver_events.iter().any(|event| matches!(
                event,
                DriverEvent::LaneClosed {
                    lane_id: LaneId::CONTROL,
                    reason
                } if *reason == expected_reason
            )));
            assert_eq!(
                driver_events.iter().any(|event| matches!(
                    event,
                    DriverEvent::DecodeError {
                        lane_id: LaneId::CONTROL,
                        kind: DecodeErrorKind::Payload,
                    }
                )),
                expect_decode_error
            );

            let transport_events = transport_events
                .lock()
                .expect("transport events mutex poisoned");
            assert_eq!(
                transport_events.iter().any(|event| matches!(
                    event,
                    TransportEvent::Closed {
                        lane_id: None,
                        reason
                    } if *reason == expected_reason
                )),
                !expect_decode_error
            );
        }

        run_case(
            std::io::Error::other("decode error: invalid Message payload"),
            ConnectionCloseReason::Protocol,
            true,
        );
        run_case(
            std::io::Error::other("connection reset by peer"),
            ConnectionCloseReason::Transport,
            false,
        );
    }

    // r[verify schema.exchange.caller]
    #[tokio::test]
    async fn caller_schema_exchange_sends_binding_once_before_request() {
        use facet::Facet;

        let sent = Arc::new(Mutex::new(Vec::new()));
        let (_connection, handle) = make_capturing_connection(Arc::clone(&sent));
        let method_id = vox_types::MethodId(700);

        let first_arg = 42_u32;
        handle
            .sender
            .send(ConnectionMessage::Request(RequestMessage {
                id: RequestId(1),
                body: RequestBody::Call(RequestCall {
                    method_id,
                    channels: Vec::new(),
                    metadata: Metadata::default(),
                    args: Payload::outgoing(&first_arg),
                    schemas: Default::default(),
                }),
            }))
            .await
            .expect("first call send");

        {
            let captured = sent.lock().expect("captured message mutex poisoned");
            assert_eq!(captured.len(), 2);
            assert_eq!(captured[0].lane_id, LaneId::CONTROL);
            match &captured[0].payload {
                CapturedPayload::Schema {
                    method_id: actual_method_id,
                    direction,
                    schemas,
                } => {
                    assert_eq!(*actual_method_id, method_id);
                    assert_eq!(*direction, BindingDirection::Args);
                    let parsed =
                        vox_phon::parse_schema_bytes(schemas).expect("parse args schema binding");
                    let expected_root =
                        vox_phon::schema_id_for_shape(<u32 as Facet>::SHAPE).expect("u32 root");
                    assert_eq!(parsed.root, expected_root);
                }
                other => panic!("expected schema message before request, got {other:?}"),
            }
            assert_eq!(captured[1].lane_id, LaneId::CONTROL);
            match &captured[1].payload {
                CapturedPayload::Call {
                    request_id,
                    method_id: actual_method_id,
                    schemas_len,
                } => {
                    assert_eq!(*request_id, RequestId(1));
                    assert_eq!(*actual_method_id, method_id);
                    assert_eq!(*schemas_len, 0);
                }
                other => panic!("expected request after schema message, got {other:?}"),
            }
        }

        let second_arg = 43_u32;
        handle
            .sender
            .send(ConnectionMessage::Request(RequestMessage {
                id: RequestId(3),
                body: RequestBody::Call(RequestCall {
                    method_id,
                    channels: Vec::new(),
                    metadata: Metadata::default(),
                    args: Payload::outgoing(&second_arg),
                    schemas: Default::default(),
                }),
            }))
            .await
            .expect("second call send");

        let captured = sent.lock().expect("captured message mutex poisoned");
        assert_eq!(captured.len(), 3);
        match &captured[2].payload {
            CapturedPayload::Call {
                request_id,
                method_id: actual_method_id,
                schemas_len,
            } => {
                assert_eq!(*request_id, RequestId(3));
                assert_eq!(*actual_method_id, method_id);
                assert_eq!(*schemas_len, 0);
            }
            other => panic!("expected second request without schema resend, got {other:?}"),
        }
    }

    // r[verify schema.exchange.callee]
    #[tokio::test]
    async fn callee_schema_exchange_sends_binding_once_before_response() {
        use facet::Facet;

        let sent = Arc::new(Mutex::new(Vec::new()));
        let (_connection, handle) = make_capturing_connection(Arc::clone(&sent));
        let method_id = vox_types::MethodId(701);
        let request_id = RequestId(11);
        handle
            .sender
            .connection_core
            .record_incoming_call(LaneId::CONTROL, request_id, method_id);

        let first_response: Result<u32, vox_types::VoxError<core::convert::Infallible>> = Ok(99);
        handle
            .sender
            .send_response(
                request_id,
                RequestResponse {
                    metadata: Metadata::default(),
                    ret: Payload::outgoing(&first_response),
                    schemas: Default::default(),
                },
            )
            .await
            .expect("first response send");

        {
            let captured = sent.lock().expect("captured message mutex poisoned");
            assert_eq!(captured.len(), 2);
            assert_eq!(captured[0].lane_id, LaneId::CONTROL);
            match &captured[0].payload {
                CapturedPayload::Schema {
                    method_id: actual_method_id,
                    direction,
                    schemas,
                } => {
                    assert_eq!(*actual_method_id, method_id);
                    assert_eq!(*direction, BindingDirection::Response);
                    let parsed = vox_phon::parse_schema_bytes(schemas)
                        .expect("parse response schema binding");
                    let expected_root = vox_phon::schema_id_for_shape(
                        <Result<
                            u32,
                            vox_types::VoxError<core::convert::Infallible>,
                        > as Facet>::SHAPE,
                    )
                    .expect("response root");
                    assert_eq!(parsed.root, expected_root);
                }
                other => panic!("expected schema message before response, got {other:?}"),
            }
            assert_eq!(captured[1].lane_id, LaneId::CONTROL);
            match &captured[1].payload {
                CapturedPayload::Response {
                    request_id: actual_request_id,
                    schemas_len,
                } => {
                    assert_eq!(*actual_request_id, request_id);
                    assert_eq!(*schemas_len, 0);
                }
                other => panic!("expected response after schema message, got {other:?}"),
            }
        }

        let second_request_id = RequestId(13);
        handle.sender.connection_core.record_incoming_call(
            LaneId::CONTROL,
            second_request_id,
            method_id,
        );
        let second_response: Result<u32, vox_types::VoxError<core::convert::Infallible>> = Ok(100);
        handle
            .sender
            .send_response(
                second_request_id,
                RequestResponse {
                    metadata: Metadata::default(),
                    ret: Payload::outgoing(&second_response),
                    schemas: Default::default(),
                },
            )
            .await
            .expect("second response send");

        let captured = sent.lock().expect("captured message mutex poisoned");
        assert_eq!(captured.len(), 3);
        match &captured[2].payload {
            CapturedPayload::Response {
                request_id: actual_request_id,
                schemas_len,
            } => {
                assert_eq!(*actual_request_id, second_request_id);
                assert_eq!(*schemas_len, 0);
            }
            other => panic!("expected second response without schema resend, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn duplicate_connection_accept_is_ignored_after_first() {
        let mut connection = make_connection();
        let conn_id = LaneId(1);
        let (result_tx, result_rx) = vox_rt::sync::oneshot::channel("connection.test.open_result");

        connection.conns.insert(
            conn_id,
            ConnectionSlot::PendingOutbound(PendingOutboundData {
                local_settings: ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 64,
                    initial_channel_credit: 16,
                },
                establishment_started_at: Instant::now(),
                result_tx: Some(result_tx),
            }),
        );

        connection.handle_inbound_accept(conn_id, accept_ref());
        let handle = result_rx
            .await
            .expect("pending outbound result should resolve")
            .expect("accept should resolve as Ok");
        assert_eq!(handle.lane_id(), conn_id);

        connection.handle_inbound_accept(conn_id, accept_ref());
        assert!(
            matches!(
                connection.conns.get(&conn_id),
                Some(ConnectionSlot::Active(LaneState { id, .. })) if *id == conn_id
            ),
            "duplicate accept should keep existing active connection state"
        );
    }

    #[tokio::test]
    async fn duplicate_connection_reject_is_ignored_after_first() {
        let mut connection = make_connection();
        let conn_id = LaneId(1);
        let (result_tx, result_rx) = vox_rt::sync::oneshot::channel("connection.test.open_result");

        connection.conns.insert(
            conn_id,
            ConnectionSlot::PendingOutbound(PendingOutboundData {
                local_settings: ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 64,
                    initial_channel_credit: 16,
                },
                establishment_started_at: Instant::now(),
                result_tx: Some(result_tx),
            }),
        );

        connection.handle_inbound_reject(conn_id, reject_ref());
        let result = result_rx
            .await
            .expect("pending outbound result should resolve");
        assert!(
            matches!(result, Err(ConnectionError::Rejected(_))),
            "expected rejection, got: {result:?}"
        );

        connection.handle_inbound_reject(conn_id, reject_ref());
        assert!(
            !connection.conns.contains_key(&conn_id),
            "duplicate reject should not recreate connection state"
        );
    }

    // r[verify rpc.flow-control.credit.initial.zero]
    #[tokio::test]
    async fn inbound_accept_with_zero_initial_credit_rejects_pending_open() {
        let mut connection = make_connection();
        let conn_id = LaneId(1);
        let (result_tx, result_rx) = vox_rt::sync::oneshot::channel("connection.test.open_result");

        connection.conns.insert(
            conn_id,
            ConnectionSlot::PendingOutbound(PendingOutboundData {
                local_settings: ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 64,
                    initial_channel_credit: 16,
                },
                establishment_started_at: Instant::now(),
                result_tx: Some(result_tx),
            }),
        );

        connection.handle_inbound_accept(conn_id, zero_credit_accept_ref());
        let result = result_rx
            .await
            .expect("pending outbound result should resolve");
        assert!(
            matches!(
                result,
                Err(ConnectionError::Protocol(ref message))
                    if message == "initial_channel_credit must be greater than zero"
            ),
            "expected zero-credit protocol error, got: {result:?}"
        );
        assert!(
            !connection.conns.contains_key(&conn_id),
            "zero-credit accept should not create an active connection"
        );
    }

    #[test]
    fn out_of_order_accept_or_reject_without_pending_is_ignored() {
        let mut connection = make_connection();
        let conn_id = LaneId(99);

        connection.handle_inbound_accept(conn_id, accept_ref());
        connection.handle_inbound_reject(conn_id, reject_ref());

        assert!(
            connection.conns.is_empty(),
            "out-of-order accept/reject should not mutate empty connection table"
        );
    }

    #[tokio::test]
    async fn close_request_clears_pending_outbound_open() {
        let mut connection = make_connection();
        let (open_result_tx, open_result_rx) =
            vox_rt::sync::oneshot::channel("connection.open.result");
        let (close_result_tx, close_result_rx) =
            vox_rt::sync::oneshot::channel("connection.close.result");

        connection.conns.insert(
            LaneId(1),
            ConnectionSlot::PendingOutbound(PendingOutboundData {
                local_settings: ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 64,
                    initial_channel_credit: 16,
                },
                establishment_started_at: Instant::now(),
                result_tx: Some(open_result_tx),
            }),
        );

        connection
            .handle_close_request(CloseRequest {
                conn_id: LaneId(1),
                metadata: vox_types::Metadata::default(),
                result_tx: close_result_tx,
            })
            .await;

        let close_result = close_result_rx
            .await
            .expect("close result should be delivered");
        assert!(
            close_result.is_ok(),
            "close should succeed for pending outbound connection"
        );

        assert!(
            open_result_rx.await.is_err(),
            "pending open result channel should be closed once the pending slot is removed"
        );
    }
}
