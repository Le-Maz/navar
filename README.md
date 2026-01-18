# Navar

**Navar** is a modular, transport-agnostic HTTP client for Rust that separates the HTTP interface from the underlying network transport and runtime.

It allows you to run standard HTTP requests over TCP, TLS, or even P2P streams (like Iroh) without changing your application logic.

## Quick Start

```rust
use navar::{Client, anyhow};
use navar_hyper::{HyperApp, Protocol};
use navar_tokio::{TokioRuntime, TokioTransport};

#[tokio::test]
async fn simple_fetch() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // 1. Create a client with your chosen Transport, App Protocol, and Runtime
    let client = Client::new(
        TokioTransport::default(),                     // Transport: TCP/TLS
        HyperApp::new().with_protocol(Protocol::Auto), // App: Auto-negotiate H1/H2
        TokioRuntime::default(),                       // Runtime: Tokio
    );

    // 2. Send requests normally
    let res = client.get("https://example.org").send().await?;

    println!("Status: {}", res.status());
    Ok(())
}
```

## Core Concepts

* **Client**: The high-level entry point that holds the configuration and dispatch logic.
* **TransportPlugin**: Defines how to connect to a remote URI (e.g., TCP, QUIC/Iroh).
* **ApplicationPlugin**: Defines the protocol handshake and session management (e.g., HTTP/1, HTTP/2).
* **AsyncRuntime**: An abstraction that allows the client to spawn background tasks on any runtime (Tokio, async-std).
