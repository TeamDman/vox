use std::sync::{Arc, Mutex};

use vox_rt::task::FutureExt;
use vox_types::{
    ChannelBinder, ChannelSink, ConnectionCloseReason, ConnectionSettings, DriverEvent,
    IncomingChannelMessage, LaneId, MethodId, Parity, Payload, ReplySink, RequestCall,
    RequestResponse, SelfRef, VoxObserver, VoxObserverHandle,
};

use super::utils::*;
use crate::Driver;
use crate::connection::{
    ConnectionError, ConnectionResourceLimits, LaneAcceptor, LaneRejectReason, LaneRejection,
    LaneRequest, PendingLane, VOX_LANE_REJECT_REASON_METADATA_KEY, acceptor_conduit,
    initiator_conduit,
};

// r[verify lane.open.wire.rejection]
// r[verify lane.open.settings]
#[tokio::test]
async fn bounded_acceptor_uses_local_limits_and_reclaims_live_lane_slot() {
    let (client_conduit, server_conduit) = message_conduit_pair();
    let mut server_handshake = test_acceptor_handshake();
    server_handshake.our_settings.initial_channel_credit = 2;
    server_handshake.our_settings.max_concurrent_requests = 3;
    let mut client_handshake = test_initiator_handshake();
    client_handshake.peer_settings = server_handshake.our_settings.clone();
    let limits = ConnectionResourceLimits {
        max_service_lanes: 1,
        max_peer_lane_opens: 4,
        max_inbound_requests_per_lane: 3,
        max_initial_channel_credit: 2,
        max_peer_channel_credit: 16,
        max_peer_concurrent_requests: 64,
        max_peer_calls_per_lane: 100,
        max_peer_channel_ids_per_lane: 8,
        max_schemas_per_lane: 8,
        max_schema_bytes_per_lane: 1_000_000,
        max_message_processing_time: std::time::Duration::from_secs(30),
    };
    let server = acceptor_conduit(server_conduit, server_handshake)
        .resource_limits(limits)
        .on_lane(EchoAcceptor)
        .establish_connection()
        .await
        .unwrap();
    let client = initiator_conduit(client_conduit, client_handshake)
        .establish_connection()
        .await
        .unwrap();
    let settings = ConnectionSettings {
        parity: Parity::Odd,
        max_concurrent_requests: 64,
        initial_channel_credit: 16,
    };
    let metadata = || vox_types::metadata().str("vox-service", "Echo").build();
    let first = client
        .open_lane_handle(settings.clone(), metadata())
        .await
        .unwrap();
    assert_eq!(first.peer_settings.max_concurrent_requests, 3);
    assert_eq!(first.peer_settings.initial_channel_credit, 2);
    let rejected = client.open_lane_handle(settings.clone(), metadata()).await;
    assert!(matches!(rejected, Err(ConnectionError::Rejected(_))));
    client
        .close_lane(first.lane_id(), Default::default())
        .await
        .unwrap();
    let replacement = client.open_lane_handle(settings, metadata()).await.unwrap();
    assert_eq!(replacement.peer_settings.initial_channel_credit, 2);
    server.shutdown().unwrap();
    client.shutdown().unwrap();
}

#[tokio::test]
async fn transport_builder_propagates_resource_limits_through_handshake() {
    let (client_link, server_link) = crate::memory_link_pair(64);
    let limits = ConnectionResourceLimits {
        max_service_lanes: 1,
        max_peer_lane_opens: 4,
        max_inbound_requests_per_lane: 3,
        max_initial_channel_credit: 2,
        max_peer_channel_credit: 16,
        max_peer_concurrent_requests: 64,
        max_peer_calls_per_lane: 100,
        max_peer_channel_ids_per_lane: 8,
        max_schemas_per_lane: 8,
        max_schema_bytes_per_lane: 1_000_000,
        max_message_processing_time: std::time::Duration::from_secs(30),
    };
    let server_task = tokio::spawn(async move {
        crate::acceptor_on(server_link)
            .max_concurrent_requests(3)
            .channel_capacity(2)
            .resource_limits(limits)
            .on_lane(EchoAcceptor)
            .establish_connection()
            .await
            .unwrap()
    });
    let client = crate::initiator_on(client_link)
        .establish_connection()
        .await
        .unwrap();
    let server = server_task.await.unwrap();
    let lane = client
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            vox_types::metadata().str("vox-service", "Echo").build(),
        )
        .await
        .unwrap();
    assert_eq!(lane.peer_settings.max_concurrent_requests, 3);
    assert_eq!(lane.peer_settings.initial_channel_credit, 2);
    server.shutdown().unwrap();
    client.shutdown().unwrap();
}

#[tokio::test]
async fn bounded_acceptor_closes_on_unknown_channel_flood() {
    struct KeepLane(Arc<Mutex<Vec<crate::LaneHandle>>>);
    impl LaneAcceptor for KeepLane {
        fn accept(&self, _: &LaneRequest, pending: PendingLane) -> Result<(), LaneRejection> {
            self.0.lock().unwrap().push(pending.into_handle());
            Ok(())
        }
    }
    let (client_conduit, server_conduit) = message_conduit_pair();
    let kept = Arc::new(Mutex::new(Vec::new()));
    let limits = ConnectionResourceLimits {
        max_service_lanes: 1,
        max_peer_lane_opens: 4,
        max_inbound_requests_per_lane: 64,
        max_initial_channel_credit: 16,
        max_peer_channel_credit: 16,
        max_peer_concurrent_requests: 64,
        max_peer_calls_per_lane: 100,
        max_peer_channel_ids_per_lane: 2,
        max_schemas_per_lane: 8,
        max_schema_bytes_per_lane: 1_000_000,
        max_message_processing_time: std::time::Duration::from_secs(30),
    };
    let server = acceptor_conduit(server_conduit, test_acceptor_handshake())
        .resource_limits(limits)
        .on_lane(KeepLane(kept.clone()))
        .establish_connection()
        .await
        .unwrap();
    let client = initiator_conduit(client_conduit, test_initiator_handshake())
        .establish_connection()
        .await
        .unwrap();
    let lane = client
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            vox_types::metadata().str("vox-service", "test").build(),
        )
        .await
        .unwrap();
    for id in [1, 3, 5] {
        let _ = lane
            .sender
            .send(crate::connection::ConnectionMessage::Channel(
                vox_types::ChannelMessage {
                    id: vox_types::ChannelId(id),
                    body: vox_types::ChannelBody::Close(vox_types::ChannelClose {
                        metadata: Default::default(),
                    }),
                },
            ))
            .await;
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), server.closed())
        .await
        .expect("resource exhaustion should close the connection");
    assert!(kept.lock().unwrap()[0].close_reason().is_some());
}

#[tokio::test]
async fn bounded_acceptor_preserves_delivery_before_receiver_registration() {
    struct KeepLane(Arc<Mutex<Option<crate::LaneHandle>>>);
    impl LaneAcceptor for KeepLane {
        fn accept(&self, _: &LaneRequest, pending: PendingLane) -> Result<(), LaneRejection> {
            *self.0.lock().unwrap() = Some(pending.into_handle());
            Ok(())
        }
    }
    let (client_conduit, server_conduit) = message_conduit_pair();
    let kept = Arc::new(Mutex::new(None));
    let limits = ConnectionResourceLimits {
        max_service_lanes: 1,
        max_peer_lane_opens: 4,
        max_inbound_requests_per_lane: 64,
        max_initial_channel_credit: 16,
        max_peer_channel_credit: 16,
        max_peer_concurrent_requests: 64,
        max_peer_calls_per_lane: 100,
        max_peer_channel_ids_per_lane: 2,
        max_schemas_per_lane: 8,
        max_schema_bytes_per_lane: 1_000_000,
        max_message_processing_time: std::time::Duration::from_secs(30),
    };
    let server = acceptor_conduit(server_conduit, test_acceptor_handshake())
        .resource_limits(limits)
        .on_lane(KeepLane(kept.clone()))
        .establish_connection()
        .await
        .unwrap();
    let client = initiator_conduit(client_conduit, test_initiator_handshake())
        .establish_connection()
        .await
        .unwrap();
    let lane = client
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            vox_types::metadata().str("vox-service", "test").build(),
        )
        .await
        .unwrap();
    let mut driver = Driver::new(kept.lock().unwrap().take().unwrap(), ());
    let caller = driver.caller();
    tokio::spawn(async move { driver.run().await });
    let channel_id = vox_types::ChannelId(1);
    let value = 123u32;
    lane.sender
        .send(crate::connection::ConnectionMessage::Channel(
            vox_types::ChannelMessage {
                id: channel_id,
                body: vox_types::ChannelBody::Item(vox_types::ChannelItem {
                    item: Payload::outgoing(&value),
                }),
            },
        ))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if caller.debug_snapshot().lanes[0]
                .open_channels
                .iter()
                .any(|channel| channel.channel_id == channel_id)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("item should be processed before receiver registration");
    let mut rx = caller.register_rx_channel(channel_id).receiver;
    let received = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let IncomingChannelMessage::Item(item) = received else {
        panic!("expected item")
    };
    let Payload::Encoded(bytes) = item.get().item else {
        panic!("expected encoded item")
    };
    assert_eq!(vox_phon::from_slice::<u32>(bytes).unwrap(), value);
    server.shutdown().unwrap();
    client.shutdown().unwrap();
}

#[tokio::test]
async fn bounded_acceptor_rejects_oversized_credit_grants() {
    struct KeepLane(Arc<Mutex<Option<crate::LaneHandle>>>);
    impl LaneAcceptor for KeepLane {
        fn accept(&self, _: &LaneRequest, pending: PendingLane) -> Result<(), LaneRejection> {
            *self.0.lock().unwrap() = Some(pending.into_handle());
            Ok(())
        }
    }
    let (client_conduit, server_conduit) = message_conduit_pair();
    let kept = Arc::new(Mutex::new(None));
    let limits = ConnectionResourceLimits {
        max_service_lanes: 1,
        max_peer_lane_opens: 4,
        max_inbound_requests_per_lane: 64,
        max_initial_channel_credit: 16,
        max_peer_channel_credit: 16,
        max_peer_concurrent_requests: 64,
        max_peer_calls_per_lane: 100,
        max_peer_channel_ids_per_lane: 2,
        max_schemas_per_lane: 8,
        max_schema_bytes_per_lane: 1_000_000,
        max_message_processing_time: std::time::Duration::from_secs(30),
    };
    let server = acceptor_conduit(server_conduit, test_acceptor_handshake())
        .resource_limits(limits)
        .on_lane(KeepLane(kept.clone()))
        .establish_connection()
        .await
        .unwrap();
    let client = initiator_conduit(client_conduit, test_initiator_handshake())
        .establish_connection()
        .await
        .unwrap();
    let lane = client
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            vox_types::metadata().str("vox-service", "test").build(),
        )
        .await
        .unwrap();
    let mut driver = Driver::new(kept.lock().unwrap().take().unwrap(), ());
    let caller = driver.caller();
    let (channel_id, sink) = caller.create_tx_channel();
    tokio::spawn(async move { driver.run().await });
    sink.send_payload(Payload::outgoing(&0u32)).await.unwrap();
    for _ in 0..2 {
        lane.sender
            .send(crate::connection::ConnectionMessage::Channel(
                vox_types::ChannelMessage {
                    id: channel_id,
                    body: vox_types::ChannelBody::GrantCredit(vox_types::ChannelGrantCredit {
                        additional: 1,
                    }),
                },
            ))
            .await
            .unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), server.closed())
        .await
        .expect("oversized grant should close the connection");
}

#[tokio::test]
async fn bounded_acceptor_enforces_local_concurrent_request_limit() {
    struct Hold;
    impl vox_types::Handler<crate::DriverReplySink> for Hold {
        async fn handle(
            &self,
            _: SelfRef<RequestCall<'static>>,
            _: crate::DriverReplySink,
            _: Arc<vox_types::SchemaRecvTracker>,
        ) {
            std::future::pending::<()>().await;
        }
    }
    struct HoldAcceptor;
    impl LaneAcceptor for HoldAcceptor {
        fn accept(&self, _: &LaneRequest, pending: PendingLane) -> Result<(), LaneRejection> {
            pending.handle_with(Hold);
            Ok(())
        }
    }
    let (client_conduit, server_conduit) = message_conduit_pair();
    let mut server_handshake = test_acceptor_handshake();
    server_handshake.our_settings.max_concurrent_requests = 1;
    let mut client_handshake = test_initiator_handshake();
    client_handshake.peer_settings = server_handshake.our_settings.clone();
    let limits = ConnectionResourceLimits {
        max_service_lanes: 1,
        max_peer_lane_opens: 4,
        max_inbound_requests_per_lane: 1,
        max_initial_channel_credit: 16,
        max_peer_channel_credit: 16,
        max_peer_concurrent_requests: 64,
        max_peer_calls_per_lane: 100,
        max_peer_channel_ids_per_lane: 2,
        max_schemas_per_lane: 8,
        max_schema_bytes_per_lane: 1_000_000,
        max_message_processing_time: std::time::Duration::from_secs(30),
    };
    let server = acceptor_conduit(server_conduit, server_handshake)
        .resource_limits(limits)
        .on_lane(HoldAcceptor)
        .establish_connection()
        .await
        .unwrap();
    let client = initiator_conduit(client_conduit, client_handshake)
        .establish_connection()
        .await
        .unwrap();
    let lane = client
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            vox_types::metadata().str("vox-service", "test").build(),
        )
        .await
        .unwrap();
    let value = 0u32;
    for id in [1, 3] {
        lane.sender
            .send(crate::connection::ConnectionMessage::Request(
                vox_types::RequestMessage {
                    id: vox_types::RequestId(id),
                    body: vox_types::RequestBody::Call(RequestCall {
                        channels: Vec::new(),
                        method_id: MethodId(1),
                        args: Payload::outgoing(&value),
                        schemas: Default::default(),
                        metadata: Default::default(),
                    }),
                },
            ))
            .await
            .unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), server.closed())
        .await
        .expect("excess concurrent requests should close the connection");
}

#[derive(Clone, Copy)]
struct ConstHandler(u32);

impl vox_types::Handler<crate::DriverReplySink> for ConstHandler {
    async fn handle(
        &self,
        _call: SelfRef<RequestCall<'static>>,
        reply: crate::DriverReplySink,
        _schemas: std::sync::Arc<vox_types::SchemaRecvTracker>,
    ) {
        let result = self.0;
        reply
            .send_reply(RequestResponse {
                ret: Payload::outgoing(&result),
                schemas: Default::default(),
                metadata: Default::default(),
            })
            .await;
    }
}

async fn call_u32(caller: &crate::Caller, value: u32) -> u32 {
    let response = caller
        .call(RequestCall {
            channels: Vec::new(),
            method_id: MethodId(1),
            args: Payload::outgoing(&value),
            schemas: Default::default(),
            metadata: Default::default(),
        })
        .await
        .expect("call should succeed");
    let response = response.get();
    let ret_bytes = match &response.ret {
        Payload::Encoded(bytes) => bytes,
        _ => panic!("expected incoming payload in response"),
    };
    vox_phon::from_slice(ret_bytes).expect("deserialize response")
}

#[derive(Default)]
struct RecordingDriverObserver {
    events: Mutex<Vec<DriverEvent>>,
}

impl RecordingDriverObserver {
    fn saw_lane_closed(&self, lane_id: LaneId) -> bool {
        self.events
            .lock()
            .expect("observer events mutex poisoned")
            .iter()
            .any(|event| {
                matches!(
                    event,
                    DriverEvent::LaneClosed { lane_id: event_lane_id, .. } if *event_lane_id == lane_id
                )
            })
    }
}

impl VoxObserver for RecordingDriverObserver {
    fn driver_event(&self, event: DriverEvent) {
        self.events
            .lock()
            .expect("observer events mutex poisoned")
            .push(event);
    }
}

// r[verify lane.open.api]
// r[verify lane.accept.api]
// r[verify lane.open.wire]
// r[verify lane.service]
// r[verify lane]
// r[verify lane.control]
// r[verify lane.open]
// r[verify lane.wire]
// r[verify lane.open.settings]
// r[verify connection.message]
// r[verify connection.message.lane-id]
#[tokio::test]
async fn open_service_lane_and_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let (client_conduit, server_conduit) = message_conduit_pair();

    let server_task = vox_rt::task::spawn(
        async move {
            acceptor_conduit(server_conduit, test_acceptor_handshake())
                .on_lane(EchoAcceptor)
                .establish_connection()
                .await
                .expect("server handshake failed")
        }
        .named("server_setup"),
    );

    let _client_caller_guard = initiator_conduit(client_conduit, test_initiator_handshake())
        .establish::<TestLaneClient>()
        .await
        .expect("client handshake failed");
    let connection_handle = _client_caller_guard.connection.clone().unwrap();

    let _server_caller_guard = server_task.await.expect("server setup failed");

    // Open a service lane.
    let lane_handle = connection_handle
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            vox_types::metadata().str("vox-service", "Echo").build(),
        )
        .await
        .expect("open service lane");
    assert!(
        !lane_handle.lane_id().is_control(),
        "service lane id should not be the control lane"
    );

    // Set up a driver on the client side for the service lane.
    let mut lane_driver = Driver::new(lane_handle, ());
    let caller = crate::Caller::new(lane_driver.caller());
    vox_rt::task::spawn(async move { lane_driver.run().await }.named("lane_client_driver"));

    // Make a call on the service lane.
    let args_value: u32 = 123;
    let response = caller
        .call(RequestCall {
            channels: Vec::new(),
            method_id: MethodId(1),
            args: Payload::outgoing(&args_value),
            schemas: Default::default(),
            metadata: Default::default(),
        })
        .await
        .expect("call should succeed");

    let response = response.get();
    let ret_bytes = match &response.ret {
        Payload::Encoded(bytes) => bytes,
        _ => panic!("expected incoming payload in response"),
    };
    let result: u32 = vox_phon::from_slice(ret_bytes).expect("deserialize response");
    assert_eq!(result, 123);
}

// r[verify lane]
// r[verify lane.service]
#[tokio::test]
async fn control_and_service_lanes_bind_separate_services() {
    let (client_conduit, server_conduit) = message_conduit_pair();

    struct ServiceAcceptor;

    impl LaneAcceptor for ServiceAcceptor {
        fn accept(
            &self,
            request: &LaneRequest,
            connection: PendingLane,
        ) -> Result<(), LaneRejection> {
            match request.service() {
                "Noop" => connection.handle_with(ConstHandler(10)),
                "Echo" => connection.handle_with(ConstHandler(20)),
                _ => return Err(LaneRejection::new(LaneRejectReason::UnknownService)),
            }
            Ok(())
        }
    }

    let server_task = vox_rt::task::spawn(
        async move {
            acceptor_conduit(server_conduit, test_acceptor_handshake())
                .on_lane(ServiceAcceptor)
                .establish_connection()
                .await
                .expect("server handshake failed")
        }
        .named("server_setup"),
    );

    let control_client_guard = initiator_conduit(client_conduit, test_initiator_handshake())
        .establish::<TestLaneClient>()
        .await
        .expect("client handshake failed");
    let connection_handle = control_client_guard.connection.clone().unwrap();
    let _server_caller_guard = server_task.await.expect("server setup failed");

    assert_eq!(call_u32(&control_client_guard.caller, 1).await, 10);

    let lane_handle = connection_handle
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            vox_types::metadata().str("vox-service", "Echo").build(),
        )
        .await
        .expect("open service lane");

    let mut lane_driver = Driver::new(lane_handle, ());
    let lane_caller = crate::Caller::new(lane_driver.caller());
    vox_rt::task::spawn(async move { lane_driver.run().await }.named("lane_client_driver"));

    assert_eq!(call_u32(&lane_caller, 2).await, 20);
    assert_eq!(call_u32(&control_client_guard.caller, 3).await, 10);
}

// r[verify lane.open.wire.rejection]
// r[verify lane.open]
// r[verify lane.open.result]
// r[verify lane.wire]
// r[verify lane.authorization.filtered]
// r[verify rejection.reason.taxonomy]
#[tokio::test]
async fn reject_service_lane() {
    let (client_conduit, server_conduit) = message_conduit_pair();

    let server_task = vox_rt::task::spawn(
        async move {
            acceptor_conduit(server_conduit, test_acceptor_handshake())
                .on_lane(crate::connection::lane_acceptor_fn(
                    |request: &LaneRequest, connection: PendingLane| match request.service() {
                        "Noop" => {
                            connection.handle_with(EchoHandler);
                            Ok(())
                        }
                        _ => Err(LaneRejection::new(LaneRejectReason::UnknownService)),
                    },
                ))
                .establish_connection()
                .await
                .expect("server handshake failed")
        }
        .named("server_setup"),
    );

    let _client_caller_guard = initiator_conduit(client_conduit, test_initiator_handshake())
        .establish::<TestLaneClient>()
        .await
        .expect("client handshake failed");
    let connection_handle = _client_caller_guard.connection.clone().unwrap();

    let _server_caller_guard = server_task.await.expect("server setup failed");

    // Try to open an unknown service lane — should be rejected.
    let result = connection_handle
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            vox_types::metadata().str("vox-service", "Unknown").build(),
        )
        .await;

    let Err(ConnectionError::Rejected(rejection)) = result else {
        panic!("expected structured rejection, got: {result:?}");
    };
    assert_eq!(rejection.reason(), LaneRejectReason::UnknownService);
    assert_eq!(
        vox_types::metadata_get_str(rejection.metadata(), VOX_LANE_REJECT_REASON_METADATA_KEY,),
        Some("unknown-service")
    );
}

// r[verify lane.open.wire.rejection]
// r[verify lane.open]
// r[verify lane.open.result]
// r[verify lane.wire]
#[tokio::test]
async fn open_service_lane_without_acceptor_is_rejected() {
    let (client_conduit, server_conduit) = message_conduit_pair();

    let server_task = vox_rt::task::spawn(
        async move {
            acceptor_conduit(server_conduit, test_acceptor_handshake())
                .establish_connection()
                .await
                .expect("server handshake failed")
        }
        .named("server_setup"),
    );

    let connection_handle = initiator_conduit(client_conduit, test_initiator_handshake())
        .establish_connection()
        .await
        .expect("client handshake failed");

    let _server_guard = server_task.await.expect("server setup failed");

    // No explicit acceptor means inbound service lanes are rejected.
    let result = connection_handle
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            vox_types::metadata().str("vox-service", "Noop").build(),
        )
        .await;

    let Err(ConnectionError::Rejected(rejection)) = result else {
        panic!("expected structured rejection, got: {result:?}");
    };
    assert_eq!(rejection.reason(), LaneRejectReason::NotReady);
}

// r[verify lane.close]
// r[verify lane.wire]
#[tokio::test]
async fn close_unknown_service_lane_is_rejected() {
    let (client_conduit, server_conduit) = message_conduit_pair();

    let server_task = vox_rt::task::spawn(
        async move {
            acceptor_conduit(server_conduit, test_acceptor_handshake())
                .establish_connection()
                .await
                .expect("server handshake failed")
        }
        .named("server_setup"),
    );

    let connection_handle = initiator_conduit(client_conduit, test_initiator_handshake())
        .establish_connection()
        .await
        .expect("client handshake failed");

    let _server_guard = server_task.await.expect("server setup failed");

    let missing_conn_id = vox_types::LaneId(1);
    let result = connection_handle
        .close_lane(missing_conn_id, Default::default())
        .await;
    assert!(
        matches!(result, Err(ConnectionError::Protocol(ref msg)) if msg == "connection not found"),
        "expected missing-connection protocol error, got: {result:?}"
    );
}

// r[verify lane.close]
// r[verify lane.close.semantics]
// r[verify lane.wire]
#[tokio::test]
async fn close_service_lane() {
    let (client_conduit, server_conduit) = message_conduit_pair();

    let server_task = vox_rt::task::spawn(
        async move {
            acceptor_conduit(server_conduit, test_acceptor_handshake())
                .on_lane(EchoAcceptor)
                .establish_connection()
                .await
                .expect("server handshake failed")
        }
        .named("server_setup"),
    );

    let _client_caller_guard = initiator_conduit(client_conduit, test_initiator_handshake())
        .establish::<TestLaneClient>()
        .await
        .expect("client handshake failed");
    let connection_handle = _client_caller_guard.connection.clone().unwrap();

    let _server_caller_guard = server_task.await.expect("server setup failed");

    // Open a service lane.
    let lane_handle = connection_handle
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            vox_types::metadata().str("vox-service", "Echo").build(),
        )
        .await
        .expect("open service lane");

    let conn_id = lane_handle.lane_id();
    assert!(
        !conn_id.is_control(),
        "service lane should not be the control lane"
    );

    // Set up a driver on the client side.
    let mut lane_driver = Driver::new(lane_handle, ());
    let caller = crate::Caller::new(lane_driver.caller());
    let caller_closed = caller.clone();
    vox_rt::task::spawn(async move { lane_driver.run().await }.named("lane_client_driver"));

    // Make a call to confirm the service lane works.
    let args_value: u32 = 42;
    let response = caller
        .call(RequestCall {
            channels: Vec::new(),
            method_id: MethodId(1),
            args: Payload::outgoing(&args_value),
            schemas: Default::default(),
            metadata: Default::default(),
        })
        .await
        .expect("call should succeed before close");

    let response = response.get();
    let ret_bytes = match &response.ret {
        Payload::Encoded(bytes) => bytes,
        _ => panic!("expected incoming payload"),
    };
    let result: u32 = vox_phon::from_slice(ret_bytes).expect("deserialize");
    assert_eq!(result, 42);

    // Close the service lane.
    connection_handle
        .close_lane(conn_id, Default::default())
        .await
        .expect("close service lane");

    tokio::time::timeout(std::time::Duration::from_secs(1), caller_closed.closed())
        .await
        .expect("caller closed() should resolve after service lane close");
    assert!(
        !caller.is_connected(),
        "caller should report disconnected after service lane close"
    );
}

// r[verify rpc.caller.liveness.last-drop-closes-connection]
// r[verify connection.lifecycle.driven]
// r[verify connection.shutdown.explicit]
#[tokio::test]
async fn dropping_last_lane_caller_does_not_close_service_lane() {
    let (client_conduit, server_conduit) = message_conduit_pair();
    let observer = Arc::new(RecordingDriverObserver::default());
    let observer_handle: VoxObserverHandle = observer.clone();

    let server_task = vox_rt::task::spawn(
        async move {
            acceptor_conduit(server_conduit, test_acceptor_handshake())
                .on_lane(EchoAcceptor)
                .establish_connection()
                .await
                .expect("server handshake failed")
        }
        .named("server_setup"),
    );

    let _client_caller_guard = initiator_conduit(client_conduit, test_initiator_handshake())
        .observer_handle(observer_handle)
        .establish::<TestLaneClient>()
        .await
        .expect("client handshake failed");
    let connection_handle = _client_caller_guard.connection.clone().unwrap();

    let _server_caller_guard = server_task.await.expect("server setup failed");

    let lane_handle = connection_handle
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            vox_types::metadata().str("vox-service", "Echo").build(),
        )
        .await
        .expect("open service lane");
    let conn_id = lane_handle.lane_id();

    let mut lane_driver = Driver::new(lane_handle, ());
    let lane_caller = crate::Caller::new(lane_driver.caller());
    vox_rt::task::spawn(async move { lane_driver.run().await }.named("lane_client_driver"));

    let response = lane_caller
        .call(RequestCall {
            channels: Vec::new(),
            method_id: MethodId(1),
            args: Payload::outgoing(&11_u32),
            schemas: Default::default(),
            metadata: Default::default(),
        })
        .await
        .expect("call should succeed before dropping lane caller");
    let response = response.get();
    let ret_bytes = match &response.ret {
        Payload::Encoded(bytes) => bytes,
        _ => panic!("expected incoming payload in response"),
    };
    let echoed: u32 = vox_phon::from_slice(ret_bytes).expect("deserialize response");
    assert_eq!(echoed, 11);

    drop(lane_caller);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !observer.saw_lane_closed(conn_id),
        "dropping the last public caller must not close the service lane"
    );

    connection_handle
        .close_lane(conn_id, Default::default())
        .await
        .expect("explicit close should close service lane");
    assert!(
        observer.saw_lane_closed(conn_id),
        "explicit close should emit the lane close event"
    );
}

// r[verify lane.close.semantics]
// r[verify rpc.channel.close]
// r[verify lane.wire]
#[tokio::test]
async fn close_service_lane_closes_registered_rx_channels() {
    let (client_conduit, server_conduit) = message_conduit_pair();

    let server_task = vox_rt::task::spawn(
        async move {
            acceptor_conduit(server_conduit, test_acceptor_handshake())
                .on_lane(EchoAcceptor)
                .establish_connection()
                .await
                .expect("server handshake failed")
        }
        .named("server_setup"),
    );

    let _client_caller_guard = initiator_conduit(client_conduit, test_initiator_handshake())
        .establish::<TestLaneClient>()
        .await
        .expect("client handshake failed");
    let connection_handle = _client_caller_guard.connection.clone().unwrap();

    let _server_caller_guard = server_task.await.expect("server setup failed");

    let lane_handle = connection_handle
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            vox_types::metadata().str("vox-service", "Echo").build(),
        )
        .await
        .expect("open service lane");

    let conn_id = lane_handle.lane_id();
    let mut lane_driver = Driver::new(lane_handle, ());
    let caller = crate::Caller::new(lane_driver.caller());
    vox_rt::task::spawn(async move { lane_driver.run().await }.named("lane_client_driver"));

    let (_channel_id, bound_rx) = caller.driver().create_rx();
    let mut rx_items = bound_rx.receiver;

    connection_handle
        .close_lane(conn_id, Default::default())
        .await
        .expect("close service lane");

    let recv_result = tokio::time::timeout(std::time::Duration::from_millis(200), rx_items.recv())
        .await
        .expect("timed out waiting for channel receiver to close");
    assert!(
        matches!(
            recv_result,
            Some(IncomingChannelMessage::ConnectionClosed(
                ConnectionCloseReason::Local
            ))
        ),
        "registered Rx channel should report connection closure when service lane closes"
    );
}

// r[verify rpc.caller.liveness.public-handle-drop]
// r[verify rpc.caller.liveness.explicit-shutdown-required]
// r[verify connection.lifecycle.driven]
// r[verify connection.shutdown.explicit]
#[tokio::test]
async fn dropping_control_and_lane_callers_does_not_shutdown_connection() {
    let (client_conduit, server_conduit) = message_conduit_pair();

    let (client_connection_task_tx, client_connection_task_rx) =
        tokio::sync::oneshot::channel::<vox_rt::task::JoinHandle<()>>();

    struct LocalEchoAcceptor;

    impl LaneAcceptor for LocalEchoAcceptor {
        fn accept(
            &self,
            _request: &LaneRequest,
            connection: PendingLane,
        ) -> Result<(), LaneRejection> {
            connection.handle_with(EchoHandler);
            Ok(())
        }
    }

    let server_task = vox_rt::task::spawn(
        async move {
            acceptor_conduit(server_conduit, test_acceptor_handshake())
                .on_lane(LocalEchoAcceptor)
                .establish_connection()
                .await
                .expect("server handshake failed")
        }
        .named("server_setup"),
    );

    let control_client = initiator_conduit(client_conduit, test_initiator_handshake())
        .spawn_fn(move |fut| {
            let handle = vox_rt::task::spawn(fut.named("client_connection_task"));
            let _ = client_connection_task_tx.send(handle);
        })
        .establish::<TestLaneClient>()
        .await
        .expect("client handshake failed");
    let connection_handle = control_client.connection.clone().unwrap();

    let server_connection = server_task.await.expect("server setup failed");
    let client_connection_task = client_connection_task_rx
        .await
        .expect("client connection handle sent");

    let lane_handle = connection_handle
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            vox_types::metadata().str("vox-service", "Echo").build(),
        )
        .await
        .expect("open service lane");

    let mut lane_driver = Driver::new(lane_handle, ());
    let lane_caller = crate::Caller::new(lane_driver.caller());
    vox_rt::task::spawn(async move { lane_driver.run().await }.named("lane_client_driver"));

    drop(control_client);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !client_connection_task.is_finished(),
        "dropping the generated caller must not close the driven connection"
    );

    let response = lane_caller
        .call(RequestCall {
            channels: Vec::new(),
            method_id: MethodId(1),
            args: Payload::outgoing(&7_u32),
            schemas: Default::default(),
            metadata: Default::default(),
        })
        .await
        .expect("service lane should still be usable after generated caller drop");
    let response = response.get();
    let ret_bytes = match &response.ret {
        Payload::Encoded(bytes) => bytes,
        _ => panic!("expected incoming payload in response"),
    };
    let echoed: u32 = vox_phon::from_slice(ret_bytes).expect("deserialize response");
    assert_eq!(echoed, 7);

    drop(lane_caller);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !client_connection_task.is_finished(),
        "dropping all public callers must not shut down the connection"
    );

    connection_handle
        .shutdown()
        .expect("client shutdown request");
    let _ = server_connection.shutdown();

    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        client_connection_task,
    )
    .await
    .expect("timed out waiting for client connection to exit")
    .expect("client connection task failed");
}
