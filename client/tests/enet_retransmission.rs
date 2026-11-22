//! A lost reliable control message is resent after ENet's round-trip
//! timeout. tokio-enet 0.1.1 waited at least 5 seconds, which held every
//! later input on the sequenced channel as long; the workspace builds the
//! vendored copy of vendor/tokio-enet, which resends as C ENet does.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use tokio_enet::{Event, Host, HostConfig, Packet, PacketMode};

const MESSAGE_SIZE: usize = 200;

/// Relays datagrams between one client and `server`, and drops the first
/// client datagram that carries the message (the only one this large).
async fn relay(server: SocketAddr, dropped: Arc<AtomicBool>) -> SocketAddr {
	let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
	let address = socket.local_addr().unwrap();
	tokio::spawn(async move {
		let mut client = None;
		let mut buffer = [0u8; 2048];
		loop {
			let (length, from) = socket.recv_from(&mut buffer).await.unwrap();
			if from == server {
				if let Some(client) = client {
					socket.send_to(&buffer[..length], client).await.unwrap();
				}
				continue;
			}
			client = Some(from);
			if length > MESSAGE_SIZE && !dropped.swap(true, Ordering::SeqCst) {
				continue;
			}
			socket.send_to(&buffer[..length], server).await.unwrap();
		}
	});
	address
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_reliable_message_arrives_within_a_second() {
	let mut server = Host::new(HostConfig { address: Some("127.0.0.1:0".parse().unwrap()), ..Default::default() }).unwrap();
	let server_address = server.local_addr().unwrap();
	let (received, received_rx) = oneshot::channel();
	tokio::spawn(async move {
		let mut received = Some(received);
		loop {
			if let Some(Event::Receive { packet, .. }) = server.service(Duration::from_millis(5)).await.unwrap() {
				if let Some(received) = received.take() {
					let _ = received.send((Instant::now(), packet.data().len()));
				}
			}
		}
	});
	let dropped = Arc::new(AtomicBool::new(false));
	let relay_address = relay(server_address, dropped.clone()).await;

	let mut client = Host::new(HostConfig { address: Some("127.0.0.1:0".parse().unwrap()), ..Default::default() }).unwrap();
	let peer = client.connect(relay_address, 1, 0).unwrap();
	let deadline = Instant::now() + Duration::from_secs(5);
	while !matches!(client.service(Duration::from_millis(5)).await.unwrap(), Some(Event::Connect { .. })) {
		assert!(Instant::now() < deadline, "the client did not connect within 5 s");
	}
	// Round trips after the connection measure the loopback's time.
	let settled = Instant::now() + Duration::from_millis(300);
	while Instant::now() < settled {
		client.service(Duration::from_millis(5)).await.unwrap();
	}

	let sent = Instant::now();
	client.peer_mut(peer).unwrap().send(0, Packet::new(&[7u8; MESSAGE_SIZE], PacketMode::ReliableSequenced)).unwrap();
	let mut received_rx = received_rx;
	let (arrived, length) = loop {
		client.service(Duration::from_millis(5)).await.unwrap();
		if let Ok(arrival) = received_rx.try_recv() {
			break arrival;
		}
		assert!(sent.elapsed() < Duration::from_secs(10), "the message did not arrive within 10 s");
	};
	assert!(dropped.load(Ordering::SeqCst), "the relay dropped no message, so nothing was resent");
	assert_eq!(length, MESSAGE_SIZE);
	let delay = arrived.duration_since(sent);
	assert!(delay < Duration::from_secs(1), "the lost message arrived after {delay:?}; ENet resends after its round-trip timeout");
}
