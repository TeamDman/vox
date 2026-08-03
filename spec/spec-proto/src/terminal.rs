//! Rust-authoritative contract for the optional terminal presentation lane.
//!
//! The terminal service uses bounded unary control messages plus one typed,
//! request-scoped frame channel. A terminal frame is a byte payload with
//! explicit dimensions and encoding; transport/runtime bounds cap its size
//! before a decoder allocates. Java can provide a local fallback when Vox is
//! unavailable.

use facet::Facet;
use vox::{Tx, service};

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

    /// Push full latest-state frames until the request is cancelled, the
    /// channel closes, or the terminal session ends.
    async fn subscribe_frames(
        &self,
        request: TerminalSubscribeRequest,
        frames: Tx<TerminalFrameEvent>,
    ) -> Result<TerminalOperationResult, TerminalError>;

    /// Return the bounded visible terminal text for deterministic probes and
    /// accessibility/debug witnesses without requiring PNG interpretation.
    async fn get_content(
        &self,
        request: TerminalContentRequest,
    ) -> Result<TerminalContentResult, TerminalError>;

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

/// Logical and physical dimensions for one terminal presentation surface.
/// Logical dimensions control PTY/VT state; physical dimensions describe the
/// target panel and the renderer's native cell metrics.
#[derive(Debug, Clone, PartialEq, Eq, Default, Facet)]
pub struct TerminalSurfaceMetrics {
    pub columns: u16,
    pub rows: u16,
    pub panel_width: u16,
    pub panel_height: u16,
    pub cell_width: u16,
    pub cell_height: u16,
    pub font_pixel_size: u16,
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
    pub selection: bool,
    pub scrollback: bool,
    pub dirty_frames: bool,
    pub max_width: u16,
    pub max_height: u16,
    pub max_frame_bytes: u32,
    #[facet(default)]
    pub backend_id: String,
    #[facet(default)]
    pub transport_id: String,
}

/// Creates a session and requests capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalConnectRequest {
    pub endpoint: String,
    pub requested_width: u16,
    pub requested_height: u16,
    #[facet(default)]
    pub surface: TerminalSurfaceMetrics,
    pub capabilities: TerminalCapabilities,
    pub client_sequence: i64,
    #[facet(default)]
    pub correlation_id: String,
}

/// Successful session creation response.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalConnectResult {
    pub session_id: String,
    pub capabilities: TerminalCapabilities,
    #[facet(default)]
    pub surface: TerminalSurfaceMetrics,
    pub server_sequence: i64,
    pub state: TerminalState,
    #[facet(default)]
    pub correlation_id: String,
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
    #[facet(default)]
    pub surface: TerminalSurfaceMetrics,
    pub client_sequence: i64,
    #[facet(default)]
    pub correlation_id: String,
}

/// Accepted logical dimensions and resulting sequence.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalResizeResult {
    pub session_id: String,
    pub width: u16,
    pub height: u16,
    #[facet(default)]
    pub surface: TerminalSurfaceMetrics,
    pub server_sequence: i64,
    #[facet(default)]
    pub correlation_id: String,
}

/// Printable text input.  The server rejects a payload longer than the
/// negotiated request bound before it mutates terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalTextInput {
    pub session_id: String,
    pub text: String,
    pub client_sequence: i64,
    #[facet(default)]
    pub correlation_id: String,
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
    #[facet(default)]
    pub correlation_id: String,
}

/// Mouse input in logical terminal coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalMouseInput {
    pub session_id: String,
    pub x: u16,
    pub y: u16,
    pub buttons: u8,
    /// Mouse button number for button transitions.
    pub button: u8,
    pub pressed: bool,
    /// True when this is a motion event rather than a button transition.
    pub motion: bool,
    pub wheel_x: i32,
    pub wheel_y: i32,
    pub client_sequence: i64,
    #[facet(default)]
    pub correlation_id: String,
}

/// Acknowledges text, key, or mouse input.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalInputResult {
    pub session_id: String,
    pub server_sequence: i64,
    pub frame_sequence: i64,
    #[facet(default)]
    pub correlation_id: String,
}

/// Requests a frame no larger than `max_frame_bytes`.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalSnapshotRequest {
    pub session_id: String,
    pub after_sequence: i64,
    pub max_frame_bytes: u32,
    pub client_sequence: i64,
    #[facet(default)]
    pub correlation_id: String,
}

/// Opens one fresh request-scoped live-frame subscription.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalSubscribeRequest {
    pub session_id: String,
    pub after_terminal_sequence: i64,
    pub max_frame_bytes: u32,
    pub client_sequence: i64,
    #[facet(default)]
    pub correlation_id: String,
}

/// Bounded producer evidence attached to each pushed frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Facet)]
pub struct TerminalPublicationTelemetry {
    pub mutations: i64,
    pub renders_started: i64,
    pub renders_completed: i64,
    pub pre_render_coalesced: i64,
    pub credit_blocked_sends: i64,
    pub frames_pushed: i64,
    pub pending_depth: u8,
    pub pending_depth_max: u8,
    pub mutation_to_send_us: i64,
    pub credit_wait_us: i64,
}

/// One authoritative terminal publication. Epochs are opaque and must only be
/// compared for equality; sequences are monotonic within a session epoch.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalFrameEvent {
    pub session_id: String,
    pub connection_epoch: String,
    pub session_epoch: String,
    pub terminal_sequence: i64,
    pub frame_sequence: i64,
    pub full_resync: bool,
    pub max_frame_bytes: u32,
    pub backend_id: String,
    pub transport_id: String,
    pub correlation_id: String,
    pub frame: TerminalSnapshot,
    #[facet(default)]
    pub publication: TerminalPublicationTelemetry,
}

/// Requests bounded visible-grid text, optionally after a known sequence.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalContentRequest {
    pub session_id: String,
    pub after_sequence: i64,
    pub max_chars: u32,
    pub client_sequence: i64,
}

/// Rust-owned visible-grid text witness.  The text includes prompt rows and
/// trailing blank rows exactly as the terminal core exposes them.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalContentResult {
    pub session_id: String,
    pub sequence: i64,
    pub text: String,
    pub complete: bool,
    pub truncated: bool,
    pub prompt: TerminalPromptMetadata,
}

/// Bounded terminal frame.  `payload` is cells, RGBA/BGRA bytes, or PNG as
/// selected by `encoding`; its length is checked against negotiated limits.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalSnapshot {
    pub session_id: String,
    pub sequence: i64,
    /// The client sequence from the request that produced this response.
    pub request_sequence: i64,
    #[facet(default)]
    pub logical_columns: u16,
    #[facet(default)]
    pub logical_rows: u16,
    pub width: u16,
    pub height: u16,
    #[facet(default)]
    pub panel_width: u16,
    #[facet(default)]
    pub panel_height: u16,
    #[facet(default)]
    pub cell_width: u16,
    #[facet(default)]
    pub cell_height: u16,
    #[facet(default)]
    pub font_pixel_size: u16,
    #[facet(default)]
    pub correlation_id: String,
    pub encoding: TerminalFrameEncoding,
    pub kind: TerminalFrameKind,
    pub stride: u32,
    pub tile_x: u16,
    pub tile_y: u16,
    pub tile_width: u16,
    pub tile_height: u16,
    pub payload: Vec<u8>,
    pub complete: bool,
    pub cursor: TerminalCursor,
    pub selection_present: bool,
    pub selection: TerminalSelection,
    pub prompt: TerminalPromptMetadata,
    #[facet(default)]
    pub timing: TerminalSnapshotTiming,
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

/// Identifies whether a snapshot replaces the complete frame or one tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum TerminalFrameKind {
    Full,
    DirtyTile,
}

/// The visible terminal cursor associated with a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct TerminalCursor {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
}

/// A linear visible-grid selection associated with a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct TerminalSelection {
    pub anchor_x: u16,
    pub anchor_y: u16,
    pub focus_x: u16,
    pub focus_y: u16,
}

/// Shell-integration markers associated with the latest visible prompt and
/// completed command. Presence flags keep this record compatible with the
/// Java 17 generator, which deliberately avoids nested Option fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct TerminalPromptMetadata {
    pub prompt_present: bool,
    pub prompt: TerminalRange,
    pub command_present: bool,
    pub command: TerminalRange,
    pub command_status_present: bool,
    pub command_status: i32,
}

/// A visible-grid range with inclusive start and exclusive end coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct TerminalRange {
    pub start_x: u16,
    pub start_y: u16,
    pub end_x: u16,
    pub end_y: u16,
}

/// Rust-side timing stages for the full-PNG snapshot that contains this
/// metadata. Java joins these values with its Vox wait and presentation
/// measurements using the enclosing snapshot correlation ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Facet)]
pub struct TerminalSnapshotTiming {
    pub pty_drain_us: i64,
    pub terminal_snapshot_us: i64,
    pub font_load_us: i64,
    pub raster_us: i64,
    pub png_encode_us: i64,
    pub total_us: i64,
}

/// Cancels a request while retaining the session.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TerminalCancelRequest {
    pub session_id: String,
    pub request_sequence: i64,
    pub reason: String,
    pub client_sequence: i64,
    #[facet(default)]
    pub correlation_id: String,
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
            request_sequence: 41,
            logical_columns: 80,
            logical_rows: 24,
            width: 80,
            height: 24,
            panel_width: 640,
            panel_height: 384,
            cell_width: 8,
            cell_height: 16,
            font_pixel_size: 16,
            correlation_id: "snapshot-01".to_string(),
            encoding: TerminalFrameEncoding::Rgba8,
            kind: TerminalFrameKind::Full,
            stride: 320,
            tile_x: 0,
            tile_y: 0,
            tile_width: 80,
            tile_height: 24,
            payload: vec![0, 16, 32, 255, 255, 128, 64, 255],
            complete: true,
            cursor: TerminalCursor {
                x: 3,
                y: 4,
                visible: true,
            },
            selection_present: true,
            selection: TerminalSelection {
                anchor_x: 1,
                anchor_y: 2,
                focus_x: 5,
                focus_y: 2,
            },
            prompt: TerminalPromptMetadata {
                prompt_present: true,
                prompt: TerminalRange {
                    start_x: 0,
                    start_y: 0,
                    end_x: 3,
                    end_y: 0,
                },
                command_present: true,
                command: TerminalRange {
                    start_x: 3,
                    start_y: 0,
                    end_x: 12,
                    end_y: 0,
                },
                command_status_present: true,
                command_status: 0,
            },
            timing: TerminalSnapshotTiming::default(),
        };
        let bytes = vox_phon::to_vec(&snapshot).expect("encode terminal snapshot");
        let decoded: TerminalSnapshot =
            vox_phon::from_slice(&bytes).expect("decode terminal snapshot");
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn terminal_snapshot_response_wire_payload_is_stable() {
        type WireResponse = Result<TerminalSnapshot, vox::VoxError<TerminalError>>;

        let snapshot = TerminalSnapshot {
            session_id: "session-01".to_string(),
            sequence: 42,
            request_sequence: 41,
            logical_columns: 80,
            logical_rows: 24,
            width: 80,
            height: 24,
            panel_width: 640,
            panel_height: 384,
            cell_width: 8,
            cell_height: 16,
            font_pixel_size: 16,
            correlation_id: "snapshot-01".to_string(),
            encoding: TerminalFrameEncoding::Rgba8,
            kind: TerminalFrameKind::Full,
            stride: 320,
            tile_x: 0,
            tile_y: 0,
            tile_width: 80,
            tile_height: 24,
            payload: vec![0, 16, 32, 255, 255, 128, 64, 255],
            complete: true,
            cursor: TerminalCursor {
                x: 3,
                y: 4,
                visible: true,
            },
            selection_present: true,
            selection: TerminalSelection {
                anchor_x: 1,
                anchor_y: 2,
                focus_x: 5,
                focus_y: 2,
            },
            prompt: TerminalPromptMetadata {
                prompt_present: true,
                prompt: TerminalRange {
                    start_x: 0,
                    start_y: 0,
                    end_x: 3,
                    end_y: 0,
                },
                command_present: true,
                command: TerminalRange {
                    start_x: 3,
                    start_y: 0,
                    end_x: 12,
                    end_y: 0,
                },
                command_status_present: true,
                command_status: 0,
            },
            timing: TerminalSnapshotTiming {
                pty_drain_us: 1,
                terminal_snapshot_us: 2,
                font_load_us: 3,
                raster_us: 4,
                png_encode_us: 5,
                total_us: 15,
            },
        };
        let payload = vox_phon::to_vec(&Ok::<_, vox::VoxError<TerminalError>>(snapshot))
            .expect("encode terminal snapshot response");
        let _: WireResponse =
            vox_phon::from_slice(&payload).expect("decode terminal snapshot response");
    }

    #[test]
    fn terminal_content_witness_round_trips_through_phon() {
        let content = TerminalContentResult {
            session_id: "session-01".to_string(),
            sequence: 42,
            text: "PS C:\\work> 100\\n".to_string(),
            complete: true,
            truncated: false,
            prompt: TerminalPromptMetadata {
                prompt_present: true,
                prompt: TerminalRange {
                    start_x: 0,
                    start_y: 0,
                    end_x: 11,
                    end_y: 0,
                },
                command_present: false,
                command: TerminalRange {
                    start_x: 0,
                    start_y: 0,
                    end_x: 0,
                    end_y: 0,
                },
                command_status_present: false,
                command_status: 0,
            },
        };
        let bytes = vox_phon::to_vec(&content).expect("encode terminal content witness");
        let decoded: TerminalContentResult =
            vox_phon::from_slice(&bytes).expect("decode terminal content witness");
        assert_eq!(decoded, content);
    }

    #[test]
    fn terminal_content_response_wire_schema_matches_generated_java_root() {
        type WireResponse = Result<TerminalContentResult, vox::VoxError<TerminalError>>;

        let schema_id = vox_phon::schema_id_for_shape(<WireResponse as Facet>::SHAPE)
            .expect("terminal content response wire schema id");
        assert_eq!(schema_id.as_u64(), 0x0152_82f6_6494_f34c);
        let schema = vox_phon::schema_bytes_for_shape(<WireResponse as Facet>::SHAPE)
            .expect("terminal content response wire schema bytes");
        assert!(!schema.is_empty(), "terminal content response schema bytes");
        let wire = Ok::<_, vox::VoxError<TerminalError>>(TerminalContentResult {
            session_id: "sfm-terminal-1".to_string(),
            sequence: 7,
            text: "PS C:\\work> hello\\n".to_string(),
            complete: true,
            truncated: false,
            prompt: TerminalPromptMetadata {
                prompt_present: true,
                prompt: TerminalRange {
                    start_x: 0,
                    start_y: 0,
                    end_x: 13,
                    end_y: 0,
                },
                command_present: false,
                command: TerminalRange {
                    start_x: 0,
                    start_y: 0,
                    end_x: 0,
                    end_y: 0,
                },
                command_status_present: false,
                command_status: 0,
            },
        });
        let payload = vox_phon::to_vec(&wire).expect("encode terminal content response");
        let decoded: WireResponse =
            vox_phon::from_slice(&payload).expect("decode terminal content response");
        assert!(matches!(
            decoded,
            Ok(TerminalContentResult { complete: true, .. })
        ));

        let error = Err::<TerminalContentResult, _>(vox::VoxError::User(Box::new(TerminalError {
            code: TerminalErrorCode::Internal,
            message: "pty output unavailable".to_string(),
            retryable: true,
            server_sequence: 8,
        })));
        let error_payload =
            vox_phon::to_vec(&error).expect("encode terminal content application error");
        let decoded_error: WireResponse = vox_phon::from_slice(&error_payload)
            .expect("decode terminal content application error");
        assert!(matches!(decoded_error, Err(vox::VoxError::User(error))
            if error.code == TerminalErrorCode::Internal
                && error.retryable
                && error.server_sequence == 8));
    }

    #[test]
    fn terminal_mouse_motion_round_trips_through_phon() {
        let input = TerminalMouseInput {
            session_id: "session-01".to_string(),
            x: 4,
            y: 5,
            buttons: 1,
            button: 0,
            pressed: true,
            motion: true,
            wheel_x: 0,
            wheel_y: 0,
            client_sequence: 7,
            correlation_id: "mouse-01".to_string(),
        };
        let bytes = vox_phon::to_vec(&input).expect("encode terminal mouse motion");
        let decoded: TerminalMouseInput =
            vox_phon::from_slice(&bytes).expect("decode terminal mouse motion");
        assert_eq!(decoded, input);
        assert!(decoded.motion);
    }

    #[test]
    fn terminal_contract_fixture_matches_method_ids_and_bounds() {
        let v1 = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test-fixtures/terminal/terminal-contract-v1.json"
        ));
        let v2 = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test-fixtures/terminal/terminal-contract-v2.json"
        ));
        let service = terminal_service_descriptor();
        assert_eq!(service.service_name, "Terminal");
        assert_eq!(service.methods.len(), 11);
        for method in service
            .methods
            .iter()
            .filter(|method| method.method_name != "subscribe_frames")
        {
            let expected = format!(
                "\"name\": \"{}\", \"id\": \"{:016x}\"",
                method.method_name, method.id.0
            );
            assert!(
                v1.contains(&expected),
                "v1 fixture is missing immutable descriptor entry: {expected}"
            );
        }
        for method in service.methods {
            let expected = format!(
                "\"name\": \"{}\", \"id\": \"{:016x}\"",
                method.method_name, method.id.0
            );
            assert!(
                v2.contains(&expected),
                "v2 fixture is missing descriptor entry: {expected}"
            );
        }
        assert!(v2.contains("\"role\": \"channel.arg.1.tx.element\""));
        assert!(v2.contains("\"producer_pending_bound\": 1"));
        assert!(v2.contains("\"coalescing\": \"newest-full-state-wins\""));
        assert!(v2.contains("\"polling\": \"recovery-only\""));
        assert!(v2.contains("\"max_width\": 512"));
        assert!(v2.contains("\"max_height\": 256"));
        assert!(v2.contains("\"max_frame_bytes\": 16777216"));
    }
}
