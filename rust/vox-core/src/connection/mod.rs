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
use vox_rt::sync::{Notify, mpsc, oneshot, watch};
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
pub use builders::*;

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
    Shutdown,
    ResourceClose,
    Close(LaneId),
    ProtocolClose {
        conn_id: LaneId,
        description: String,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum FailureDisposition {
    Cancelled,
    Indeterminate,
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
        send_drop_control(&self.control_tx, DropControlRequest::Shutdown)
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
    failures: Arc<mpsc::UnboundedSender<(RequestId, FailureDisposition)>>,
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
            )
            .await
            .map_err(|_| ())
    }

    pub(crate) async fn send_channel_with_writer_schema<'a>(
        &self,
        channel: ChannelMessage<'a>,
        writer_schema: Option<vox_types::ChannelWriterSchemaPlan>,
    ) -> Result<(), ()> {
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
        )
    }

    /// Send a received connection message without re-materializing payload values.
    pub(crate) async fn send_owned(
        &self,
        schemas: Arc<vox_types::SchemaRecvTracker>,
        msg: SelfRef<ConnectionMessage<'static>>,
    ) -> Result<(), ()> {
        let msg_ref = msg.get();
        let payload = match msg_ref {
            ConnectionMessage::Request(request) => MessagePayload::RequestMessage(RequestMessage {
                id: request.id,
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
            .send(
                Message {
                    lane_id: self.lane_id,
                    payload,
                },
                None,
                Some(&*schemas),
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
    pub fn mark_failure(&self, request_id: RequestId, disposition: FailureDisposition) {
        let _ = self.failures.send((request_id, disposition));
    }
}

pub struct LaneHandle {
    pub(crate) sender: ConnectionSender,
    pub(crate) rx: mpsc::Receiver<RecvMessage>,
    pub(crate) failures_rx: mpsc::UnboundedReceiver<(RequestId, FailureDisposition)>,
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

/// Forward all request/channel traffic between two connections.
///
/// This is a protocol-level bridge: it does not inspect service schemas or method IDs.
/// It exits when either side closes or a forward send fails, then requests closure of
/// both underlying connections.
pub async fn proxy_lanes(left: LaneHandle, right: LaneHandle) -> Result<(), ConnectionError> {
    if left.parity == right.parity {
        return Err(ConnectionError::Protocol(
            "proxy_lanes requires opposite parities".into(),
        ));
    }
    let left_conn_id = left.lane_id();
    let right_conn_id = right.lane_id();
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
                if right_sender
                    .send_owned(recv.schemas, recv.msg)
                    .await
                    .is_err()
                {
                    break;
                }
            }
            ProxyEvent::Right(Some(recv)) => {
                if left_sender
                    .send_owned(recv.schemas, recv.msg)
                    .await
                    .is_err()
                {
                    break;
                }
            }
            ProxyEvent::Left(None) | ProxyEvent::Right(None) => break,
        }
    }

    if let Some(tx) = left_control_tx.as_ref() {
        let _ = send_drop_control(tx, DropControlRequest::Close(left_conn_id));
    }
    if let Some(tx) = right_control_tx.as_ref() {
        let _ = send_drop_control(tx, DropControlRequest::Close(right_conn_id));
    }
    Ok(())
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
        let connection_core = Arc::new(ConnectionCore {
            inner: std::sync::Mutex::new(ConnectionCoreInner {
                tx: Some(Arc::new(tx) as Arc<dyn DynConduitTx>),
                conns: HashMap::new(),
            }),
            outbound_tx,
            admission: vox_rt::sync::Mutex::new("connection.outbound_admission", false),
            admission_stopped: std::sync::atomic::AtomicBool::new(false),
            admission_stopped_tx,
            admission_stopped_rx,
            #[cfg(test)]
            before_admission_commit: std::sync::Mutex::new(None),
            observer: observer.clone(),
            channel_gates: std::sync::Mutex::new(HashMap::new()),
        });
        let outbound_abort = spawn_outbound_worker(outbound_rx);
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
            conns: BTreeMap::new(),
            conn_ids: IdAllocator::new(Parity::Odd), // overwritten in establish_as_*
            lane_acceptor,
            open_rx,
            close_rx,
            control_tx,
            control_rx,
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

        Ok(self.make_control_lane_handle(result.our_settings, result.peer_settings))
    }

    fn make_control_lane_handle(
        &mut self,
        local_settings: ConnectionSettings,
        peer_settings: ConnectionSettings,
    ) -> LaneHandle {
        self.make_connection_handle(LaneId::CONTROL, local_settings, peer_settings)
    }

    fn make_connection_handle(
        &mut self,
        conn_id: LaneId,
        local_settings: ConnectionSettings,
        peer_settings: ConnectionSettings,
    ) -> LaneHandle {
        let label = format!("connection.lane{}", conn_id.0);
        let (conn_tx, conn_rx) = mpsc::channel::<RecvMessage>(&label, 64);
        let (failures_tx, failures_rx) = mpsc::unbounded_channel(format!("{label}.failures"));
        let (closed_tx, closed_rx) = watch::channel(None);
        let sender = ConnectionSender {
            lane_id: conn_id,
            connection_core: Arc::clone(&self.connection_core),
            failures: Arc::new(failures_tx),
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
                schema_recv_tracker: Arc::new(vox_types::SchemaRecvTracker::new()),
                lane_grant: Arc::clone(&lane_grant_state),
                peer_channel_ids: HashSet::new(),
                received_schema_count: 0,
                received_schema_bytes: 0,
                peer_calls: 0,
            }),
        );

        LaneHandle {
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
        }
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
                            #[cfg(unix)]
                            let processing = self.handle_message(msg, fds, &mut keepalive_runtime);
                            #[cfg(not(unix))]
                            let processing = self.handle_message(msg, (), &mut keepalive_runtime);
                            if let Some(limits) = limits {
                                if vox_types::time::tokio::timeout(
                                    limits.max_message_processing_time,
                                    processing,
                                )
                                .await
                                .is_err()
                                {
                                    self.resource_exhausted = true;
                                }
                            } else {
                                processing.await;
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
                    self.handle_open_request(req).await;
                }
                RunEvent::Close(Some(req)) => {
                    self.handle_close_request(req).await;
                }
                RunEvent::Control(Some(req)) => {
                    if !self.handle_drop_control_request(req).await {
                        self.close_all_connections(ConnectionCloseReason::Local);
                        break;
                    }
                }
                RunEvent::Keepalive => {
                    if !self.handle_keepalive_tick(&mut keepalive_runtime).await {
                        self.close_all_connections(ConnectionCloseReason::Protocol);
                        break;
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
            let mut inner = self
                .connection_core
                .inner
                .lock()
                .expect("connection core mutex poisoned");
            inner.conns.clear();
            let tx = inner.tx.take();
            drop(inner);
            if let Some(tx) = tx {
                tx.abort();
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
                // Record incoming calls so ConnectionCore::send() can look up
                // the method_id when sending the response.
                if let RequestBody::Call(call) = &r_ref.body {
                    self.connection_core.record_incoming_call(conn_id, r_ref.id, call.method_id);
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
                let _ = send_drop_control(&self.control_tx, DropControlRequest::Shutdown);
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
        let handle = self.make_connection_handle(
            conn_id,
            our_settings.clone(),
            open.connection_settings.clone(),
        );

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
                self.conns.remove(&conn_id);
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
                self.conns.remove(&conn_id);
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
        let slot = self.remove_connection(&conn_id);
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
                let mut handle = self.make_connection_handle(
                    conn_id,
                    pending.local_settings.clone(),
                    accept.connection_settings.clone(),
                );
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
        let slot = self.remove_connection(&conn_id);
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

    async fn handle_drop_control_request(&mut self, req: DropControlRequest) -> bool {
        match req {
            DropControlRequest::Shutdown => {
                trace!("connection shutdown requested");
                // Partial shutdown gate: close admission promptly. Connection
                // drain, protocol close and transport teardown remain separate.
                if vox_types::time::tokio::timeout(
                    Duration::from_secs(1),
                    self.connection_core.stop_outbound_admission(),
                )
                .await
                .is_err()
                {
                    self.connection_core.force_stop_admission();
                    self.outbound_abort.abort();
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

                if self
                    .remove_connection_with_reason(&conn_id, ConnectionCloseReason::Local)
                    .is_some()
                {
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

    fn remove_connection(&mut self, conn_id: &LaneId) -> Option<ConnectionSlot> {
        self.remove_connection_with_reason(conn_id, ConnectionCloseReason::Unknown)
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
        if let Some(ConnectionSlot::Active(state)) = &slot {
            let _ = state.closed_tx.send(Some(reason));
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
                let _ = state.closed_tx.send(Some(reason));
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
struct ChannelGate {
    opened: std::sync::atomic::AtomicBool,
    notify: Notify,
}

pub(crate) struct ConnectionCore {
    inner: std::sync::Mutex<ConnectionCoreInner>,
    outbound_tx: mpsc::Sender<OutboundBatch>,
    admission: vox_rt::sync::Mutex<bool>,
    admission_stopped: std::sync::atomic::AtomicBool,
    admission_stopped_tx: watch::Sender<bool>,
    admission_stopped_rx: watch::Receiver<bool>,
    #[cfg(test)]
    before_admission_commit: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    observer: Option<VoxObserverHandle>,
    /// Open gates for channels the local side opened but whose declaring Call has not
    /// yet been enqueued. Channel IDs are allocated per lane, so both are needed.
    channel_gates: std::sync::Mutex<HashMap<(LaneId, vox_types::ChannelId), Arc<ChannelGate>>>,
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

struct OutboundBatch {
    conn_id: LaneId,
    request_id: Option<RequestId>,
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
);

struct PrepareOutboundError {
    gated_channels: Vec<vox_types::ChannelId>,
}

async fn run_outbound_worker(mut rx: mpsc::Receiver<OutboundBatch>) {
    while let Some(batch) = rx.recv().await {
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
        let _ = batch.result_tx.send(result);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_outbound_worker(rx: mpsc::Receiver<OutboundBatch>) -> futures_util::future::AbortHandle {
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    let worker = async move {
        let _ = futures_util::future::Abortable::new(run_outbound_worker(rx), registration).await;
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
fn spawn_outbound_worker(rx: mpsc::Receiver<OutboundBatch>) -> futures_util::future::AbortHandle {
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    wasm_bindgen_futures::spawn_local(async move {
        let _ = futures_util::future::Abortable::new(run_outbound_worker(rx), registration).await;
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
    fn admission_is_stopped(&self) -> bool {
        self.admission_stopped
            .load(std::sync::atomic::Ordering::Acquire)
    }

    fn force_stop_admission(&self) {
        self.admission_stopped
            .store(true, std::sync::atomic::Ordering::Release);
        let _ = self.admission_stopped_tx.send(true);
    }

    async fn wait_admission_stopped(&self) {
        let mut stopped = self.admission_stopped_rx.clone();
        if *stopped.borrow() {
            return;
        }
        while stopped.changed().await.is_ok() {
            if *stopped.borrow() {
                return;
            }
        }
    }

    async fn stop_outbound_admission(&self) {
        let mut closed = self.admission.lock().await;
        *closed = true;
        self.force_stop_admission();
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
    pub(crate) fn register_channel_gate(&self, lane_id: LaneId, channel_id: vox_types::ChannelId) {
        self.channel_gates
            .lock()
            .expect("channel gates mutex poisoned")
            .entry((lane_id, channel_id))
            .or_insert_with(|| {
                Arc::new(ChannelGate {
                    opened: std::sync::atomic::AtomicBool::new(false),
                    notify: Notify::new("connection.channel_gate"),
                })
            });
    }

    /// Open the gates for `channels` (the channels a just-enqueued Call declared),
    /// releasing any parked channel-item sends so they reach the wire AFTER the Call.
    fn open_channel_gates(&self, lane_id: LaneId, channels: &[vox_types::ChannelId]) {
        if channels.is_empty() {
            return;
        }
        let mut gates = self
            .channel_gates
            .lock()
            .expect("channel gates mutex poisoned");
        for id in channels {
            if let Some(gate) = gates.remove(&(lane_id, *id)) {
                gate.opened
                    .store(true, std::sync::atomic::Ordering::Release);
                gate.notify.notify_waiters();
            }
        }
    }

    /// Wait until channel `channel_id`'s declaring Call has been enqueued (its gate is
    /// open), if a gate exists. No gate (or an already-open one) returns immediately —
    /// so a non-channel item, or an item whose Call already went out, never blocks.
    async fn await_channel_gate(&self, lane_id: LaneId, channel_id: vox_types::ChannelId) {
        let gate = {
            let gates = self
                .channel_gates
                .lock()
                .expect("channel gates mutex poisoned");
            match gates.get(&(lane_id, channel_id)) {
                Some(gate) => Arc::clone(gate),
                None => return,
            }
        };
        loop {
            if gate.opened.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            let notified = gate.notify.notified();
            if gate.opened.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    /// Whether channel `channel_id` may send now: true if it has no gate (not a
    /// locally-opened, not-yet-declared channel) or its gate is already open.
    fn channel_gate_open(&self, lane_id: LaneId, channel_id: vox_types::ChannelId) -> bool {
        self.channel_gates
            .lock()
            .expect("channel gates mutex poisoned")
            .get(&(lane_id, channel_id))
            .is_none_or(|gate| gate.opened.load(std::sync::atomic::Ordering::Acquire))
    }

    fn prepare_outbound_batch<'a>(
        &self,
        mut msg: Message<'a>,
        binder: Option<&'a dyn vox_types::ChannelBinder>,
        forwarded_schemas: Option<&vox_types::SchemaRecvTracker>,
        channel_method: Option<&'static vox_types::MethodDescriptor>,
        extra_schema_sends: Vec<PendingSchemaSend>,
    ) -> Result<PreparedOutboundBatch, PrepareOutboundError> {
        let conn_id = msg.lane_id;
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
                                conn_state_guard.inflight_incoming.remove(&req.id)
                                && let Some(schema_send) = Self::plan_response_schema_send(
                                    &mut conn_state_guard,
                                    req.id,
                                    method_id,
                                    resp,
                                    forwarded_schemas,
                                )
                            {
                                schema_sends.push(schema_send);
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
                    return Err(PrepareOutboundError { gated_channels });
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
                return Err(PrepareOutboundError { gated_channels });
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
                request_id,
                payload_kind,
                conn_state,
                tx,
                schema_sends,
                payload_send,
                result_tx,
            },
            result_rx,
            gated_channels,
        ))
    }

    // r[impl schema.principles.sender-driven]
    pub(crate) async fn send<'a>(
        &self,
        msg: Message<'a>,
        binder: Option<&'a dyn vox_types::ChannelBinder>,
        forwarded_schemas: Option<&vox_types::SchemaRecvTracker>,
    ) -> Result<(), ()> {
        self.send_with_options(msg, binder, forwarded_schemas, None, Vec::new(), |_| {})
            .await
    }

    async fn send_with_options<'a>(
        &self,
        msg: Message<'a>,
        binder: Option<&'a dyn vox_types::ChannelBinder>,
        forwarded_schemas: Option<&vox_types::SchemaRecvTracker>,
        channel_method: Option<&'static vox_types::MethodDescriptor>,
        extra_schema_sends: Vec<PendingSchemaSend>,
        declared_channels: impl FnOnce(&[vox_types::ChannelId]),
    ) -> Result<(), ()> {
        let lane_id = msg.lane_id;
        if self.admission_is_stopped() {
            declared_channels(&[]);
            return Err(());
        }
        // r[impl rpc.channel.item] Hold an outbound channel item/close until the Call
        // that opened its channel has been enqueued — the sender upholds frame order.
        if let Some(channel_id) = gated_channel_id(&msg) {
            let gate = self.await_channel_gate(lane_id, channel_id).fuse();
            let stopped = self.wait_admission_stopped().fuse();
            futures_util::pin_mut!(gate, stopped);
            futures_util::select_biased! {
                () = stopped => { declared_channels(&[]); return Err(()); },
                () = gate => {},
            }
        }
        let (batch, result_rx, gated_channels) = match self.prepare_outbound_batch(
            msg,
            binder,
            forwarded_schemas,
            channel_method,
            extra_schema_sends,
        ) {
            Ok(prepared) => prepared,
            Err(err) => {
                self.open_channel_gates(lane_id, &err.gated_channels);
                declared_channels(&err.gated_channels);
                return Err(());
            }
        };
        declared_channels(&gated_channels);
        let queued = async {
            let reserve = self.outbound_tx.clone().reserve_owned().fuse();
            let stopped = self.wait_admission_stopped().fuse();
            futures_util::pin_mut!(reserve, stopped);
            let permit = futures_util::select_biased! {
                () = stopped => return Err(()),
                permit = reserve => permit.map_err(|_| ())?,
            };
            let admission = self.admission.lock().fuse();
            let stopped = self.wait_admission_stopped().fuse();
            futures_util::pin_mut!(admission, stopped);
            let guard = futures_util::select_biased! {
                () = stopped => return Err(()),
                guard = admission => guard,
            };
            if *guard || self.admission_is_stopped() {
                return Err(());
            }
            // A forced stop can race after this check and before enqueue. Such
            // a preadmitted batch has an uncertain outcome, not a hard cutoff.
            #[cfg(test)]
            if let Some(hook) = self.before_admission_commit.lock().unwrap().take() {
                hook();
            }
            drop(permit.send(batch));
            Ok::<(), ()>(())
        }
        .await;
        // This Call is now on the queue (or the queue is gone): release its channels'
        // gates so parked items follow it — unconditionally, so a failed enqueue never
        // strands a parked `tx.send`.
        self.open_channel_gates(lane_id, &gated_channels);
        if queued.is_err() {
            if let Some(observer) = &self.observer {
                observer.driver_event(vox_types::DriverEvent::OutboundQueueClosed { lane_id });
            }
            return Err(());
        }
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
    fn try_send_with_options<'a>(
        &self,
        msg: Message<'a>,
        binder: Option<&'a dyn vox_types::ChannelBinder>,
        forwarded_schemas: Option<&vox_types::SchemaRecvTracker>,
        channel_method: Option<&'static vox_types::MethodDescriptor>,
        extra_schema_sends: Vec<PendingSchemaSend>,
    ) -> Result<(), TrySendError<()>> {
        let lane_id = msg.lane_id;
        if self.admission_is_stopped() {
            return Err(TrySendError::Closed(()));
        }
        // r[impl rpc.channel.item] A channel item whose declaring Call hasn't been
        // enqueued yet can't go out (frame order); signal backpressure so the caller
        // retries — the async `send` path parks instead.
        if let Some(channel_id) = gated_channel_id(&msg)
            && !self.channel_gate_open(lane_id, channel_id)
        {
            return Err(TrySendError::Full(()));
        }
        let (batch, _result_rx, gated_channels) = match self.prepare_outbound_batch(
            msg,
            binder,
            forwarded_schemas,
            channel_method,
            extra_schema_sends,
        ) {
            Ok(prepared) => prepared,
            Err(err) => {
                self.open_channel_gates(lane_id, &err.gated_channels);
                return Err(TrySendError::Closed(()));
            }
        };
        let guard = match self.admission.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                self.open_channel_gates(lane_id, &gated_channels);
                return Err(if self.admission_is_stopped() {
                    TrySendError::Closed(())
                } else {
                    TrySendError::Full(())
                });
            }
        };
        if *guard || self.admission_is_stopped() {
            drop(guard);
            self.open_channel_gates(lane_id, &gated_channels);
            return Err(TrySendError::Closed(()));
        }
        // As above, a forced stop can overlap this already-admitted try-send.
        #[cfg(test)]
        if let Some(hook) = self.before_admission_commit.lock().unwrap().take() {
            hook();
        }
        let result = self.outbound_tx.try_send(batch).map_err(|err| match err {
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
        // Release this Call's channel gates now that it's been handed to the queue.
        self.open_channel_gates(lane_id, &gated_channels);
        result
    }

    /// Record that an incoming call was received, so we can look up the
    /// method_id when sending the response.
    pub(crate) fn record_incoming_call(
        &self,
        conn_id: LaneId,
        request_id: RequestId,
        method_id: vox_types::MethodId,
    ) {
        let mut inner = self.inner.lock().expect("connection core mutex poisoned");
        let conn_state = get_or_create_send_conn_state(&mut inner, conn_id);
        vox_types::dlog!(
            "[schema] record_incoming_call: conn={:?} req={:?} method={:?}",
            conn_id,
            request_id,
            method_id
        );
        conn_state
            .lock()
            .expect("send conn state mutex poisoned")
            .inflight_incoming
            .insert(request_id, method_id);
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
        request_id: RequestId,
        method_id: vox_types::MethodId,
        response: &mut RequestResponse<'_>,
    ) {
        let mut inner = self.inner.lock().expect("connection core mutex poisoned");
        let conn_state = get_or_create_send_conn_state(&mut inner, conn_id);
        let mut conn_state = conn_state.lock().expect("send conn state mutex poisoned");
        let key = (vox_types::BindingDirection::Response, method_id);
        if conn_state
            .send_tracker
            .has_sent_binding(method_id, vox_types::BindingDirection::Response)
        {
            response.schemas = Default::default();
            return;
        }

        let prepared = match &response.ret {
            vox_types::Payload::Value { shape, .. } => {
                match Self::get_or_plan_binding_for_shape(
                    &mut conn_state,
                    key,
                    request_id,
                    "response",
                    shape,
                ) {
                    Some(prepared) => prepared,
                    None => return,
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
    /// erased-error-response path). Commits directly (best-effort dedup,
    /// `r[schema.exchange]`).
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
        let mut conn_state = conn_state.lock().expect("send conn state mutex poisoned");
        if conn_state
            .send_tracker
            .has_sent_binding(method_id, vox_types::BindingDirection::Response)
        {
            response.schemas = Default::default();
            return;
        }
        match vox_types::SchemaSendTracker::plan_for_shape(shape) {
            Ok(prepared) => {
                response.schemas = conn_state.send_tracker.commit_prepared_plan(
                    method_id,
                    vox_types::BindingDirection::Response,
                    prepared,
                );
            }
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
        request_id: RequestId,
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
            conn_state
                .planned_bindings
                .get(&key)
                .cloned()
                .unwrap_or_else(|| vox_types::PreparedSchemaPlan {
                    bytes: response.schemas.0.clone(),
                })
        } else {
            match &response.ret {
                vox_types::Payload::Value { shape, .. } => Self::get_or_plan_binding_for_shape(
                    conn_state, key, request_id, "response", shape,
                )?,
                vox_types::Payload::Encoded(_) => {
                    let Some(source) = forwarded_schemas else {
                        tracing::error!(
                            "schema attachment failed: missing forwarded response schemas for method {:?}",
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
        conn_state.inflight_outgoing.insert(request_id, method_id);
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

    struct PendingRx;

    #[tokio::test]
    async fn unsupported_type_erased_conduit_does_not_claim_shared_close() {
        let shared: Arc<dyn DynConduitTx> = Arc::new(CapturingTx {
            sent: Arc::new(Mutex::new(Vec::new())),
        });
        let error = shared
            .close_shared_bounded(Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);

        use vox_types::Conduit;
        let (link, _peer) = crate::memory_link_pair(4);
        let (tx, _rx) = crate::BareConduit::<MessageFamily, _>::new(link).split();
        let shared: Arc<dyn DynConduitTx> = Arc::new(tx);
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
        connection.remove_connection_with_reason(&LaneId(2), ConnectionCloseReason::Local);
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
    #[ignore = "diagnostic: explicit shutdown currently retains the conduit behind a held lane"]
    async fn shutdown_releases_conduit_when_service_lane_is_retained() {
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
            .send(DropControlRequest::Shutdown)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), connection.run())
            .await
            .unwrap();
        drop(connection);
        assert!(
            core.inner.lock().unwrap().tx.is_none(),
            "explicit shutdown retained the conduit sender behind a held service lane"
        );
        drop(lanes);
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
            .send(DropControlRequest::Shutdown)
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
    async fn channel_gates_for_equal_ids_on_distinct_lanes_are_independent() {
        let (connection, _lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let core = connection.connection_core.clone();
        let channel_id = vox_types::ChannelId(3);
        // Each lane's Driver has its own allocator, so both can allocate this ID.
        let first_lane = LaneId(1);
        let second_lane = LaneId(3);
        core.register_channel_gate(first_lane, channel_id);
        core.register_channel_gate(second_lane, channel_id);
        let first_core = core.clone();
        let first = tokio::spawn(async move {
            first_core.await_channel_gate(first_lane, channel_id).await;
        });
        let second_core = core.clone();
        let mut second = tokio::spawn(async move {
            second_core
                .await_channel_gate(second_lane, channel_id)
                .await;
        });
        core.open_channel_gates(first_lane, &[channel_id]);
        tokio::time::timeout(Duration::from_secs(2), first)
            .await
            .unwrap()
            .unwrap();
        assert!(core.channel_gate_open(first_lane, channel_id));
        assert!(
            !core.channel_gate_open(second_lane, channel_id),
            "opening one lane must not open another lane's gate"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut second)
                .await
                .is_err()
        );
        core.open_channel_gates(second_lane, &[channel_id]);
        tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .unwrap()
            .unwrap();
        assert!(core.channel_gate_open(second_lane, channel_id));
        connection.outbound_abort.abort();
    }

    #[tokio::test]
    async fn shutdown_wakes_producer_waiting_for_channel_gate() {
        let (mut connection, lane) = make_capturing_connection(Arc::new(Mutex::new(Vec::new())));
        let sender = lane.sender.clone();
        let channel_id = vox_types::ChannelId(3);
        connection
            .connection_core
            .register_channel_gate(sender.lane_id(), channel_id);
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
            .send(DropControlRequest::Shutdown)
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
            .send(DropControlRequest::Shutdown)
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
        let first_stranded = tokio::time::timeout(Duration::from_millis(50), &mut first)
            .await
            .is_err();
        let second_stranded = tokio::time::timeout(Duration::from_millis(50), &mut second)
            .await
            .is_err();
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
        let _first = tokio::time::timeout(Duration::from_secs(2), first)
            .await
            .unwrap();
        let _second = tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .unwrap();
        connection.outbound_abort.abort();
        assert!(
            !first_stranded && !second_stranded && !stale_request,
            "cancelled call left waiters stranded: {first_stranded}/{second_stranded}, stale request: {stale_request}"
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
        control_tx.send(DropControlRequest::Shutdown).unwrap();
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
