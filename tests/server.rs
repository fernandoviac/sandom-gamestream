use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use sandom_gamestream::{Server, ServerConfig, ServerPorts};

fn free_port() -> u16 {
	TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[test]
fn serverinfo_names_the_host_and_advertises_h264_only() {
	let certificate_directory = tempfile::tempdir().unwrap();
	let ports = ServerPorts {
		http: free_port(),
		https: free_port(),
		rtsp: free_port(),
		video: free_port(),
		control: free_port(),
		audio: free_port(),
	};
	let server = Server::start(
		ServerConfig {
			host_name: "sandom-test-host".to_string(),
			certificate_directory: certificate_directory.path().to_path_buf(),
			application_title: "Sandom".to_string(),
			bind_address: "127.0.0.1".to_string(),
			ports,
		},
		|_| {},
	)
	.unwrap();

	let mut connection = TcpStream::connect(("127.0.0.1", ports.http)).unwrap();
	write!(
		connection,
		"GET /serverinfo?uniqueid=0123456789ABCDEF HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
	)
	.unwrap();
	let mut response = String::new();
	connection.read_to_string(&mut response).unwrap();

	assert!(response.starts_with("HTTP/1.1 200"), "{response}");
	assert!(response.contains("<hostname>sandom-test-host</hostname>"), "{response}");
	assert!(
		response.contains("<ServerCodecModeSupport>1</ServerCodecModeSupport>"),
		"{response}"
	);
	assert!(response.contains(&format!("<HttpsPort>{}</HttpsPort>", ports.https)), "{response}");
	assert!(response.contains("<PairStatus>0</PairStatus>"), "{response}");
	assert!(certificate_directory.path().join("cert.pem").exists());

	server.stop().unwrap();
}
