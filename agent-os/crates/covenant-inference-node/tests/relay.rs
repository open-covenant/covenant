use std::net::SocketAddr;

use covenant_inference_node::frame::{read_frame, write_frame};
use covenant_inference_node::target::LocalHttpTarget;
use covenant_inference_node::tunnel::{
    serve_connection, signed_registration, RelayOpen, RelayReady, RelayService,
};
use covenant_inference_protocol::TunnelRegistration;
use ed25519_dalek::SigningKey;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn key() -> SigningKey {
    let mut seed = [0_u8; 32];
    getrandom::getrandom(&mut seed).unwrap();
    SigningKey::from_bytes(&seed)
}

/// A local TCP echo server standing in for the inference engine.
async fn spawn_echo() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0_u8; 4096];
                loop {
                    match socket.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if socket.write_all(&buffer[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    addr
}

#[tokio::test]
async fn relay_splices_the_gateway_to_the_local_target() {
    let echo = spawn_echo().await;
    let target = LocalHttpTarget::new(echo);
    let key = key();
    let registration = signed_registration("slot-1", &key).unwrap();

    // The duplex pipe stands in for the post-mTLS gateway connection.
    let (mut gateway, node) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let mut node = node;
        serve_connection(&mut node, &registration, &target).await
    });

    let received: TunnelRegistration = read_frame(&mut gateway).await.unwrap();
    assert!(received.verify(&key.verifying_key()).is_ok());

    write_frame(
        &mut gateway,
        &RelayOpen {
            service: RelayService::Inference,
        },
    )
    .await
    .unwrap();
    let ready: RelayReady = read_frame(&mut gateway).await.unwrap();
    assert!(ready.ready, "target should have accepted the relay");

    // Bytes written on the gateway side reach the echo target and come back.
    gateway.write_all(b"ping inference").await.unwrap();
    let mut echoed = [0_u8; 14];
    gateway.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"ping inference");

    drop(gateway);
    server.await.unwrap().unwrap();
}
