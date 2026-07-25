//! Rust-authoritative contract for the optional terminal presentation lane.
//!
//! The terminal service deliberately uses bounded, unary messages for the
//! first Java/Rust slice.  A terminal frame is a byte payload with explicit
//! dimensions and encoding; transport/runtime bounds cap its size before a
//! decoder allocates.  The service does not require channels, file
//! descriptors, or a remote UI toolkit, so Java can provide a local fallback
//! when Vox is unavailable.

use facet::Facet;
use vox::service;

/// Terminal lifecycle and presentation service.
#[service]
pub trait Terminal {
    /// Negotiate capabilities and create a bounded terminal session.
    async fn connect(
        &self,
        request: TerminalConnectRequest,
    ) -> Result<TerminalConnectResult, TerminalError>;

    /// Return the capabilities currently accepted for a session.
    async fn capabilities(
        &self,
        request: TerminalCapabilitiesRequest,
    ) -> Result<TerminalCapabilitiesResult, TerminalError>;

    /// Resize the session's logical terminal surface.
    async fn resize(
        &self,
        request: TerminalResizeRequest,
    ) -> Result<TerminalResizeResult, TerminalError>;

    /// Deliver printable text input to a session.
    async fn send_text(
        &self,
        request: TerminalTextInput,
    ) -> Result<TerminalInputResult, TerminalError>;

    /// Deliver a key press/release or repeat event to a session.
    async fn send_key(
        &self,
        request: TerminalKeyInput,
    ) -> Result<TerminalInputResult, TerminalError>;

    /// Deliver a mouse position/button/wheel event to a session.
    async fn send_mouse(
        &self,
        request: TerminalMouseInput,
    ) -> Result<TerminalInputResult, TerminalError>;

    /// Request the latest bounded frame, optionally after a known sequence.
    async fn snapshot(
        &self,
        request: TerminalSnapshotRequest,
    ) -> Result<TerminalSnapshot, TerminalError>;

    /// Cancel an in-flight terminal operation without closing the session.
    async fn cancel(
        &self,
        request: TerminalCancelRequest,
    ) -> Result<TerminalOperationResult, TerminalError>;

    /// Close a session and release its server-side resources.
    async fn disconnect(
        &self,
        request: TerminalDisconnectRequest,
    ) -> Result<TerminalOperationResult, TerminalError>;
}

/// Negotiated feature and resource limits.  A peer must treat the maxima as
/// hard bounds; it may advertise smaller values in its response.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalCapabilities {
    pub text_input: bool,
    pub key_input: bool,
    pub mouse_input: bool,
    pub structured_cells: bool,
    pub raster_frames: bool,
    pub max_width: u16,
    pub max_height: u16,
    pub max_frame_bytes: u32,
}

/// Creates a session and requests capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalConnectRequest {
    pub endpoint: String,
    pub requested_width: u16,
    pub requested_height: u16,
    pub capabilities: TerminalCapabilities,
    pub client_sequence: i64,
}

/// Successful session creation response.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalConnectResult {
    pub session_id: String,
    pub capabilities: TerminalCapabilities,
    pub width: u16,
    pub height: u16,
    pub server_sequence: i64,
    pub state: TerminalState,
}

/// Queries the negotiated capabilities for a session.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalCapabilitiesRequest {
    pub session_id: String,
    pub client_sequence: i64,
}

/// Capability response with sequence metadata.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalCapabilitiesResult {
    pub session_id: String,
    pub capabilities: TerminalCapabilities,
    pub server_sequence: i64,
}

/// Changes logical terminal dimensions.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalResizeRequest {
    pub session_id: String,
    pub width: u16,
    pub height: u16,
    pub client_sequence: i64,
}

/// Accepted logical dimensions and resulting sequence.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalResizeResult {
    pub session_id: String,
    pub width: u16,
    pub height: u16,
    pub server_sequence: i64,
}

/// Printable text input.  The server rejects a payload longer than the
/// negotiated request bound before it mutates terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalTextInput {
    pub session_id: String,
    pub text: String,
    pub client_sequence: i64,
}

/// Key input, with a stable numeric key code and modifier bitset.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalKeyInput {
    pub session_id: String,
    pub key_code: u32,
    pub modifiers: u32,
    pub pressed: bool,
    pub repeat: bool,
    pub client_sequence: i64,
}

/// Mouse input in logical terminal coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalMouseInput {
    pub session_id: String,
    pub x: u16,
    pub y: u16,
    pub buttons: u8,
    pub wheel_x: i32,
    pub wheel_y: i32,
    pub client_sequence: i64,
}

/// Acknowledges text, key, or mouse input.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalInputResult {
    pub session_id: String,
    pub server_sequence: i64,
    pub frame_sequence: i64,
}

/// Requests a frame no larger than `max_frame_bytes`.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalSnapshotRequest {
    pub session_id: String,
    pub after_sequence: i64,
    pub max_frame_bytes: u32,
    pub client_sequence: i64,
}

/// Bounded terminal frame.  `payload` is cells, RGBA/BGRA bytes, or PNG as
/// selected by `encoding`; its length is checked against negotiated limits.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalSnapshot {
    pub session_id: String,
    pub sequence: i64,
    pub width: u16,
    pub height: u16,
    pub encoding: TerminalFrameEncoding,
    pub payload: Vec<u8>,
    pub complete: bool,
}

/// Frame payload representation negotiated by the peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum TerminalFrameEncoding {
    StructuredCells,
    Rgba8,
    Bgra8,
    Png,
}

/// Cancels a request while retaining the session.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalCancelRequest {
    pub session_id: String,
    pub request_sequence: i64,
    pub reason: String,
    pub client_sequence: i64,
}

/// Closes the session or records a peer disconnect.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalDisconnectRequest {
    pub session_id: String,
    pub reason: String,
    pub client_sequence: i64,
}

/// Acknowledges cancellation or disconnection.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalOperationResult {
    pub session_id: String,
    pub state: TerminalState,
    pub server_sequence: i64,
}

/// Lifecycle state visible to both peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum TerminalState {
    Ready,
    Closing,
    Closed,
    Disconnected,
}

/// Structured application-level failure.  Transport errors remain Vox
/// infrastructure failures and are not encoded as this type.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalError {
    pub code: TerminalErrorCode,
    pub message: String,
    pub retryable: bool,
    pub server_sequence: i64,
}

/// Stable terminal error categories for user-visible fallback behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum TerminalErrorCode {
    InvalidRequest,
    UnsupportedCapability,
    SessionNotFound,
    CapacityExceeded,
    Cancelled,
    Disconnected,
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_snapshot_round_trips_through_phon() {
        let snapshot = TerminalSnapshot {
            session_id: "session-01".to_string(),
            sequence: 42,
            width: 80,
            height: 24,
            encoding: TerminalFrameEncoding::Rgba8,
            payload: vec![0, 16, 32, 255, 255, 128, 64, 255],
            complete: true,
        };
        let bytes = vox_phon::to_vec(&snapshot).expect("encode terminal snapshot");
        let decoded: TerminalSnapshot =
            vox_phon::from_slice(&bytes).expect("decode terminal snapshot");
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn terminal_contract_fixture_matches_method_ids_and_bounds() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test-fixtures/terminal/terminal-contract-v1.json"
        ));
        let service = terminal_service_descriptor();
        assert_eq!(service.service_name, "Terminal");
        assert_eq!(service.methods.len(), 9);
        for method in service.methods {
            let expected = format!(
                "\"name\": \"{}\", \"id\": \"{:016x}\"",
                method.method_name, method.id.0
            );
            assert!(
                fixture.contains(&expected),
                "fixture is missing descriptor entry: {expected}"
            );
        }
        assert!(fixture.contains("\"max_width\": 512"));
        assert!(fixture.contains("\"max_height\": 256"));
        assert!(fixture.contains("\"max_frame_bytes\": 16777216"));
    }
}
