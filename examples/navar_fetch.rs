use clap::Parser;
use http::{Method, Uri};
use http_body_util::BodyExt;
use navar::{AsyncRuntime, Client, application::ApplicationPlugin, transport::TransportPlugin};
use navar_h3::H3App;
use navar_hyper::HyperApp;
use navar_iroh::IrohTransport;
use navar_tokio::{TokioRuntime, TokioTransport};

/// A simple curl-like HTTP client using `navar`.
/// Supports standard HTTP/HTTPS via TCP as well as `iroh+http` and `iroh+h3`.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Config {
    /// The URL to request
    uri: Uri,

    /// HTTP method to use
    #[arg(short = 'X', long, default_value_t = Method::GET)]
    method: Method,

    /// Custom headers (can be used multiple times)
    #[arg(short = 'H', long)]
    headers: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Config::parse();

    // 1. Install default Rustls provider (for standard HTTPS)
    let _ = rustls::crypto::ring::default_provider().install_default();

    // 2. Determine scheme to select the correct transport/app plugin
    let scheme = args.uri.scheme_str().unwrap_or("http");

    match scheme {
        // Standard TCP/TLS Transport
        "http" | "https" => {
            let transport = TokioTransport::default();
            let app = HyperApp::default();
            let runtime = TokioRuntime::default();
            let client = Client::new(transport, app, runtime);

            exec_request(client, &args).await?;
        }

        // Iroh Transport with Hyper (HTTP/1.1 or HTTP/2)
        "iroh+http" => {
            let endpoint = create_iroh_endpoint().await?;
            // ALPN must match what the server expects
            let transport = IrohTransport::new(endpoint, vec![b"iroh+http".to_vec()]);
            let app = HyperApp::default();
            let runtime = TokioRuntime::default();
            let client = Client::new(transport, app, runtime);

            exec_request(client, &args).await?;
        }

        // Iroh Transport with H3 (HTTP/3)
        "iroh+h3" => {
            let endpoint = create_iroh_endpoint().await?;
            let transport = IrohTransport::new(endpoint, vec![b"iroh+h3".to_vec()]);
            let app = H3App::new();
            let runtime = TokioRuntime::default();
            let client = Client::new(transport, app, runtime);

            exec_request(client, &args).await?;
        }

        _ => {
            eprintln!("Unsupported scheme: {}", scheme);
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Helper to initialize a standard Iroh Endpoint
async fn create_iroh_endpoint() -> anyhow::Result<iroh::Endpoint> {
    let endpoint = iroh::Endpoint::builder().bind().await?;
    endpoint.online().await;
    Ok(endpoint)
}

/// Generic execution logic for any Navar Client configuration
async fn exec_request<T, A, R>(client: Client<T, A, R>, args: &Config) -> anyhow::Result<()>
where
    R: AsyncRuntime,
    T: TransportPlugin,
    A: ApplicationPlugin<T::Conn>,
{
    // Build the request
    let mut req_builder = client.request(args.method.clone(), args.uri.clone());

    // Add headers if provided
    for header in &args.headers {
        if let Some((k, v)) = header.split_once(':') {
            req_builder = req_builder.header(k.trim(), v.trim());
        }
    }

    // Send request
    let response = req_builder.send().await?;

    let (res_parts, res_body) = response.into_parts();

    // Print response headers
    println!("{res_parts:#?}");

    // Print response body as UTF-8 string
    let res_body = res_body.collect().await?;
    println!("{}", String::from_utf8_lossy(&res_body.to_bytes()));

    Ok(())
}
