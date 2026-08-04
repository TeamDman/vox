use std::sync::{Arc, Mutex};
use std::time::Duration;

use vox::{Rx, Tx};

#[derive(Clone, Debug, facet::Facet)]
#[repr(u8)]
pub enum AttachError {
    Rejected,
}

#[vox::service]
trait BulkChannelStash {
    async fn attach(&self, input: Rx<Vec<u64>>) -> Result<(), AttachError>;
}

const ITEMS: u64 = 65;
const WORDS_PER_ITEM: usize = 8192;

#[derive(Clone)]
struct BulkChannelDrainService;

impl BulkChannelStash for BulkChannelDrainService {
    async fn attach(&self, mut input: Rx<Vec<u64>>) -> Result<(), AttachError> {
        for expected in 0..ITEMS {
            let received = input
                .recv()
                .await
                .map_err(|_| AttachError::Rejected)?
                .ok_or(AttachError::Rejected)?;
            received.map(|item| {
                assert_eq!(item.first().copied(), Some(expected));
                assert_eq!(item.len(), WORDS_PER_ITEM);
            });
        }
        Ok(())
    }
}

#[vox::service]
trait ChannelClosureProbe {
    async fn wait_for_receiver_close(&self, output: Tx<u32>) -> bool;
}

#[derive(Clone)]
struct ChannelClosureProbeService {
    started: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl ChannelClosureProbe for ChannelClosureProbeService {
    async fn wait_for_receiver_close(&self, output: Tx<u32>) -> bool {
        if let Some(started) = self.started.lock().expect("started mutex poisoned").take() {
            let _ = started.send(());
        }
        output.closed().await;
        output.is_closed()
    }
}

// r[verify rpc.channel.delivery.reliable]
#[tokio::test]
async fn memory_reliable_rx_argument_delivers_large_item_burst_before_response() {
    let (client_link, server_link) = vox::memory_link_pair(64);
    let service = BulkChannelDrainService;

    let server = tokio::spawn(async move {
        let client = vox::acceptor_on(server_link)
            .channel_capacity(64)
            .keepalive(vox::ConnectionKeepaliveConfig {
                ping_interval: Duration::from_secs(5),
                pong_timeout: Duration::from_secs(30),
            })
            .on_lane(BulkChannelStashDispatcher::new(service))
            .establish_connection()
            .await
            .expect("server establish");
        client.closed().await;
    });

    let client = vox::initiator_on(client_link)
        .channel_capacity(64)
        .keepalive(vox::ConnectionKeepaliveConfig {
            ping_interval: Duration::from_secs(5),
            pong_timeout: Duration::from_secs(30),
        })
        .establish::<BulkChannelStashClient>()
        .await
        .expect("client establish")
        .with_middleware(vox::ClientLogging::default());

    let (tx, rx) = vox::channel::<Vec<u64>>();
    let attach_client = client.clone();
    let attach_task = tokio::spawn(async move {
        attach_client
            .attach(rx)
            .await
            .expect("attach should drain channel before responding");
    });

    let sender = tokio::spawn(async move {
        for value in 0..ITEMS {
            let mut item = vec![value; WORDS_PER_ITEM];
            item[WORDS_PER_ITEM - 1] = ITEMS - value;
            tx.send(item)
                .await
                .expect("send failed during large-item burst");
        }
    });

    tokio::select! {
        sender_result = sender => {
            sender_result.expect("sender task failed");
        }
        _ = tokio::time::sleep(Duration::from_secs(10)) => {
            panic!("large-item burst did not complete");
        }
    }
    tokio::time::timeout(Duration::from_secs(10), attach_task)
        .await
        .expect("attach did not complete after large-item burst")
        .expect("attach task panicked");

    drop(client);
    server.abort();
}

// r[verify rpc.channel.lifecycle]
// r[verify rpc.channel.reset]
#[tokio::test]
async fn memory_rpc_tx_closed_wakes_when_peer_rx_is_dropped() {
    let (client_link, server_link) = vox::memory_link_pair(16);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let service = ChannelClosureProbeService {
        started: Arc::new(Mutex::new(Some(started_tx))),
    };

    let server = tokio::spawn(async move {
        let connection = vox::acceptor_on(server_link)
            .channel_capacity(16)
            .on_lane(ChannelClosureProbeDispatcher::new(service))
            .establish_connection()
            .await
            .expect("server establish");
        connection.closed().await;
    });

    let client = vox::initiator_on(client_link)
        .channel_capacity(16)
        .establish::<ChannelClosureProbeClient>()
        .await
        .expect("client establish");

    let (tx, rx) = vox::channel::<u32>();
    let probe_client = client.clone();
    let call = tokio::spawn(async move { probe_client.wait_for_receiver_close(tx).await });

    tokio::time::timeout(Duration::from_secs(1), started_rx)
        .await
        .expect("handler did not receive Tx")
        .expect("handler start signal dropped");
    assert!(
        !call.is_finished(),
        "Tx::closed should remain pending while the peer Rx is live"
    );

    drop(rx);

    let closed = tokio::time::timeout(Duration::from_secs(1), call)
        .await
        .expect("callee Tx::closed did not observe dropped peer Rx")
        .expect("probe call task panicked")
        .expect("probe RPC failed");
    assert!(closed, "callee should observe the channel as terminal");

    drop(client);
    server.abort();
}
