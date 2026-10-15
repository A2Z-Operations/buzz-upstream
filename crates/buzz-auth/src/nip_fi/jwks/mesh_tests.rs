//! Regression tests for the explicit deployment-authenticated JWKS transport.

use super::*;

fn minimal_jwks_json(kid: &str) -> String {
    format!(
        r#"{{"keys":[{{"kty":"EC","crv":"P-256","x":"f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU","y":"x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0","use":"sig","alg":"ES256","kid":"{kid}"}}]}}"#
    )
}

fn mesh_contract(uri: &str) -> JwksSourceContract {
    JwksSourceContract::new_deployment_authenticated(uri.to_owned(), 300, 3600)
        .expect("valid mesh contract")
}

#[test]
fn deployment_authenticated_contract_rejects_invalid_uri_and_timing() {
    for uri in [
        "https://mesh.example.test/jwks.json",
        "http://127.0.0.1/jwks.json",
        "http://[::1]/jwks.json",
        "http://2130706433/jwks.json",
        "http://0177.0.0.1/jwks.json",
        "http://0x7f000001/jwks.json",
        "http://user:password@mesh.example.test/jwks.json",
        "http://@mesh.example.test/jwks.json",
        "http://mesh.example.test/jwks.json?token=private",
        "http://mesh.example.test/jwks.json?",
        "http://mesh.example.test/jwks.json#fragment",
        "http://mesh.example.test/jwks.json#",
        "http:\\\\mesh.example.test\\jwks.json",
    ] {
        assert!(
            JwksSourceContract::new_deployment_authenticated(uri.to_owned(), 300, 3600).is_none(),
            "mesh contract must reject {uri}"
        );
    }

    let uri = "http://mesh.example.test/jwks.json".to_owned();
    for (refresh, deadline) in [
        (0, 3600),
        (300, 0),
        (3600, 3600),
        (300, 301),
        (1, MAX_JWKS_TIMING_SECONDS + 1),
    ] {
        assert!(
            JwksSourceContract::new_deployment_authenticated(uri.clone(), refresh, deadline)
                .is_none(),
            "mesh contract must reject timing ({refresh}, {deadline})"
        );
    }

    assert!(
        JwksSourceContract::new(uri, 300, 3600).is_none(),
        "the public constructor must keep rejecting HTTP"
    );
}

#[test]
fn mesh_contract_canonicalizes_and_transport_participates_in_equality() {
    let canonical = mesh_contract("http://mesh.example.test/a/jwks.json");
    let equivalent = mesh_contract("HTTP://MESH.EXAMPLE.TEST:80/a/./jwks.json");
    assert_eq!(canonical.jwks_uri(), "http://mesh.example.test/a/jwks.json");
    assert_eq!(canonical, equivalent);

    let public = JwksSourceContract::new(
        "https://mesh.example.test/a/jwks.json".to_owned(),
        300,
        3600,
    )
    .expect("valid public contract");
    let mut transport_mismatch = public.clone();
    transport_mismatch.transport = JwksTransport::Mesh;
    assert_ne!(
        public, transport_mismatch,
        "the sealed contract equality must include transport"
    );
}

#[test]
fn resolved_address_policy_separates_public_https_from_mesh_http() {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    let ip = |v4: [u8; 4]| IpAddr::V4(Ipv4Addr::from(v4));
    let global = ip([8, 8, 8, 8]);
    let private = ip([10, 20, 30, 40]);
    let reserved_240 = ip([240, 0, 0, 1]);
    let global_v6 = IpAddr::V6("2606:4700::1".parse().expect("valid public IPv6"));
    let unclassified_v6 = IpAddr::V6("fe00::1".parse().expect("valid IPv6 address"));
    let loopback_v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);

    for address in [private, reserved_240, loopback_v6] {
        assert_eq!(
            validate_resolved_addresses(&[address], JwksTransport::Mesh),
            Ok(address),
            "mesh transport may pin an explicitly non-global address"
        );
    }
    assert_eq!(
        validate_resolved_addresses(&[global], JwksTransport::Mesh),
        Err(JwksFetchError::InvalidUri)
    );
    assert_eq!(
        validate_resolved_addresses(&[global_v6], JwksTransport::Mesh),
        Err(JwksFetchError::InvalidUri)
    );
    assert_eq!(
        validate_resolved_addresses(&[unclassified_v6], JwksTransport::Mesh),
        Err(JwksFetchError::InvalidUri),
        "mesh transport must reject addresses outside the known non-global classes"
    );
    assert_eq!(
        validate_resolved_addresses(&[private, global], JwksTransport::Mesh),
        Err(JwksFetchError::InvalidUri),
        "mixed mesh/global DNS answers must fail closed"
    );
    assert_eq!(
        validate_resolved_addresses(&[global, private], JwksTransport::PublicHttps),
        Err(JwksFetchError::InvalidUri),
        "public HTTPS keeps rejecting any private DNS answer"
    );
    assert_eq!(
        validate_resolved_addresses(&[reserved_240], JwksTransport::PublicHttps),
        Err(JwksFetchError::InvalidUri),
        "public HTTPS keeps rejecting the mesh virtual-address range"
    );
    assert_eq!(
        validate_resolved_addresses(&[], JwksTransport::Mesh),
        Err(JwksFetchError::NetworkError)
    );
}

async fn bind_localhost_listener() -> tokio::net::TcpListener {
    use std::net::ToSocketAddrs;

    let listener = tokio::task::spawn_blocking(|| {
        let address = ("localhost", 0)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| std::io::Error::other("localhost has no addresses"))?;
        let listener = std::net::TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        Ok::<_, std::io::Error>(listener)
    })
    .await
    .expect("localhost listener task")
    .expect("bind first localhost address");
    tokio::net::TcpListener::from_std(listener).expect("register listener with Tokio")
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt as _;

    let mut bytes = Vec::new();
    let mut chunk = [0; 1024];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk).await.expect("read request");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    String::from_utf8(bytes).expect("HTTP request headers are ASCII")
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    headers: &str,
    body: &str,
) {
    use tokio::io::AsyncWriteExt as _;

    let response = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .expect("write response");
}

#[tokio::test]
async fn production_source_fetches_mesh_contract_and_raw_fetch_stays_public_only() {
    let listener = bind_localhost_listener().await;
    let port = listener.local_addr().expect("listener address").port();
    let uri = format!("http://localhost:{port}/.well-known/jwks.json");
    let contract = mesh_contract(&uri);
    let issuer = "https://issuer.example";
    let public_issuer = "https://public-issuer.example";
    let public_contract = JwksSourceContract::new(
        format!("https://localhost:{port}/public-jwks.json"),
        300,
        3600,
    )
    .expect("public HTTPS hostname contract");
    let source = ProductionJwksSource::new(
        vec![
            IssuerJwksConfig {
                issuer: issuer.to_owned(),
                contract: contract.clone(),
            },
            IssuerJwksConfig {
                issuer: public_issuer.to_owned(),
                contract: public_contract,
            },
        ],
        HttpJwksFetcher::new(),
    )
    .expect("valid mixed-transport source");
    let body = minimal_jwks_json("mesh-key");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept JWKS request");
        let request = read_http_request(&mut stream).await;
        write_http_response(
            &mut stream,
            "200 OK",
            "Content-Type: application/json\r\n",
            &body,
        )
        .await;
        (listener, request)
    });

    let snapshot = source
        .get_snapshot(issuer)
        .await
        .expect("production source warms its snapshot through the mesh contract");
    assert_eq!(snapshot.issuer(), issuer);
    let (listener, request) = server.await.expect("local JWKS server task");
    assert!(
        request.starts_with("GET /.well-known/jwks.json HTTP/1.1\r\n"),
        "the fetcher uses GET at the configured path: {request:?}"
    );

    assert_eq!(
        HttpJwksFetcher::new().fetch_jwks(&uri).await.unwrap_err(),
        JwksFetchError::InvalidUri,
        "raw fetches cannot inherit the contract's HTTP permission"
    );
    assert!(
        JwksSourceContract::new(uri, 300, 3600).is_none(),
        "the public constructor cannot create an HTTP contract"
    );

    tokio::select! {
        snapshot = source.get_snapshot(public_issuer) => {
            assert!(snapshot.is_none(), "public HTTPS must reject localhost DNS");
        }
        accepted = listener.accept() => {
            panic!("public HTTPS contract attempted to connect to its non-global DNS result: {accepted:?}");
        }
    }
}

#[tokio::test]
async fn production_mesh_fetcher_denies_redirects_and_non_success_statuses() {
    let listener = bind_localhost_listener().await;
    let port = listener.local_addr().expect("listener address").port();
    let uri = format!("http://localhost:{port}/jwks.json");
    let contract = mesh_contract(&uri);
    let target_body = minimal_jwks_json("redirect-target");
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.expect("accept first request");
        let first_request = read_http_request(&mut first).await;
        write_http_response(
            &mut first,
            "302 Found",
            &format!("Location: http://localhost:{port}/target\r\n"),
            "",
        )
        .await;

        // If redirects are followed, this second response makes that behavior
        // observable as a successful fetch instead of a network failure.
        let (mut second, _) = listener.accept().await.expect("accept redirect target");
        let second_request = read_http_request(&mut second).await;
        write_http_response(&mut second, "200 OK", "", &target_body).await;
        (first_request, second_request)
    });

    assert_eq!(
        HttpJwksFetcher::new()
            .fetch_for_contract(&contract)
            .await
            .unwrap_err(),
        JwksFetchError::NetworkError,
        "3xx responses are rejected without following Location"
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());

    let listener = bind_localhost_listener().await;
    let port = listener.local_addr().expect("listener address").port();
    let uri = format!("http://localhost:{port}/jwks.json");
    let contract = mesh_contract(&uri);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept non-2xx request");
        let request = read_http_request(&mut stream).await;
        write_http_response(&mut stream, "503 Service Unavailable", "", "").await;
        request
    });
    assert_eq!(
        HttpJwksFetcher::new()
            .fetch_for_contract(&contract)
            .await
            .unwrap_err(),
        JwksFetchError::NetworkError
    );
    let request = server.await.expect("non-2xx server task");
    assert!(request.starts_with("GET /jwks.json HTTP/1.1\r\n"));
}

#[tokio::test]
async fn production_mesh_fetcher_keeps_response_cap() {
    let listener = bind_localhost_listener().await;
    let port = listener.local_addr().expect("listener address").port();
    let uri = format!("http://localhost:{port}/jwks.json");
    let contract = mesh_contract(&uri);
    let oversized_body = vec![b'x'; MAX_JWKS_RESPONSE_BYTES + 1];
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept oversized request");
        let request = read_http_request(&mut stream).await;
        use tokio::io::AsyncWriteExt as _;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
            .await
            .expect("write response headers");
        stream
            .write_all(&oversized_body)
            .await
            .expect("write oversized streamed body");
        request
    });
    assert_eq!(
        HttpJwksFetcher::new()
            .fetch_for_contract(&contract)
            .await
            .unwrap_err(),
        JwksFetchError::ResponseTooLarge
    );
    let request = server.await.expect("oversized response server task");
    assert!(request.starts_with("GET /jwks.json HTTP/1.1\r\n"));
}

#[tokio::test]
async fn production_mesh_fetcher_disables_environment_proxies() {
    use std::ffi::OsString;
    static PROXY_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    struct RestoreEnv(Vec<(&'static str, Option<OsString>)>);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    let _guard = PROXY_ENV_LOCK.lock().await;
    let names = [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ];
    let restore = RestoreEnv(
        names
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect(),
    );
    for name in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        std::env::set_var(name, "http://127.0.0.1:1");
    }
    for name in ["HTTPS_PROXY", "https_proxy", "NO_PROXY", "no_proxy"] {
        std::env::remove_var(name);
    }

    let listener = bind_localhost_listener().await;
    let port = listener.local_addr().expect("listener address").port();
    let uri = format!("http://localhost:{port}/jwks.json");
    let contract = mesh_contract(&uri);
    let body = minimal_jwks_json("no-proxy-key");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept local request");
        let request = read_http_request(&mut stream).await;
        write_http_response(&mut stream, "200 OK", "", &body).await;
        request
    });
    let response = HttpJwksFetcher::new()
        .fetch_for_contract(&contract)
        .await
        .expect("mesh HTTP request bypasses configured proxy");
    assert!(response.contains("no-proxy-key"));
    let request = server.await.expect("local server task");
    assert!(request.starts_with("GET /jwks.json HTTP/1.1\r\n"));
    drop(restore);
}
