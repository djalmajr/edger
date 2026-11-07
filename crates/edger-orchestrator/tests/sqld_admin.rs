//! (EDG-54 / 14a) Cliente administrativo SQLD: provas por TCP local real contra
//! um servidor controlado (sem SQLD real, sem produção). Todo servidor é
//! abortado no `Drop` (inclusive em falha) e todo filho é limitado e morto.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use edger_orchestrator::sqld_admin::{
    parse_remote_public_key, SqldAdminClient, SqldAdminConfig, SqldAdminError, SqldCreateOutcome,
    SqldNamespaceConfig, SqldRemoteKey, MAX_ADMIN_RESPONSE_BYTES, MAX_CONFIG_TIMEOUT,
};
use edger_orchestrator::sqld_registry::{SqldRegistryRecord, SqldRegistryState};
use spki::der::asn1::{BitString, Null};
use spki::der::pem::LineEnding;
use spki::der::{Any, EncodePem};
use spki::{AlgorithmIdentifierOwned, ObjectIdentifier, SubjectPublicKeyInfoOwned};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};
use zeroize::Zeroizing;

const KEY_CANARY: &str = "ADMINKEYCANARY0123";
const BODY_CANARY: &str = "BODYCANARY0123";
const JWT_CANARY: &str = "eyJCANARYJWT.payload.sig";

// ---------------------------------------------------------------- fixture

#[derive(Clone, Debug)]
struct Req {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

enum Reply {
    Full {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    },
    /// Sem `Content-Length`: corpo em chunks e fecha a conexão.
    Stream {
        status: u16,
        chunks: Vec<Vec<u8>>,
    },
    /// Escreve os bytes crus e nunca mais responde.
    Hang {
        raw: Vec<u8>,
    },
    Delay(Duration, Box<Reply>),
}

fn ok(body: &str) -> Reply {
    status_body(200, body)
}

fn status_body(status: u16, body: &str) -> Reply {
    Reply::Full {
        status,
        headers: vec![("Content-Type", "application/json".into())],
        body: body.as_bytes().to_vec(),
    }
}

type Handler = Arc<dyn Fn(&Req) -> Reply + Send + Sync>;

struct Server {
    port: u16,
    reqs: Arc<Mutex<Vec<Req>>>,
    max_inflight: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        // Aborta o accept; o JoinSet das conexões é derrubado junto.
        self.task.abort();
    }
}

impl Server {
    async fn start(handler: impl Fn(&Req) -> Reply + Send + Sync + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let reqs = Arc::new(Mutex::new(Vec::new()));
        let inflight = Arc::new(AtomicUsize::new(0));
        let max_inflight = Arc::new(AtomicUsize::new(0));
        let handler: Handler = Arc::new(handler);
        let (r, i, m) = (reqs.clone(), inflight, max_inflight.clone());
        let task = tokio::spawn(async move {
            let mut conns = JoinSet::new();
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                conns.spawn(serve_conn(
                    stream,
                    handler.clone(),
                    r.clone(),
                    i.clone(),
                    m.clone(),
                ));
            }
        });
        Server {
            port,
            reqs,
            max_inflight,
            task,
        }
    }

    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn requests(&self) -> Vec<Req> {
        self.reqs.lock().unwrap().clone()
    }

    fn count(&self) -> usize {
        self.reqs.lock().unwrap().len()
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Req> {
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        let mut tmp = [0u8; 2048];
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let path = first.next()?.to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let mut tmp = [0u8; 2048];
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(len);
    Some(Req {
        method,
        path,
        headers,
        body,
    })
}

async fn serve_conn(
    mut stream: TcpStream,
    handler: Handler,
    reqs: Arc<Mutex<Vec<Req>>>,
    inflight: Arc<AtomicUsize>,
    max: Arc<AtomicUsize>,
) {
    while let Some(req) = read_request(&mut stream).await {
        let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
        max.fetch_max(now, Ordering::SeqCst);
        reqs.lock().unwrap().push(req.clone());
        let reply = handler(&req);
        let keep = write_reply(&mut stream, reply).await;
        inflight.fetch_sub(1, Ordering::SeqCst);
        if !keep {
            break;
        }
    }
}

async fn write_reply(stream: &mut TcpStream, reply: Reply) -> bool {
    let mut reply = reply;
    while let Reply::Delay(d, inner) = reply {
        tokio::time::sleep(d).await;
        reply = *inner;
    }
    match reply {
        Reply::Full {
            status,
            headers,
            body,
        } => {
            let mut out = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\n", body.len());
            for (k, v) in headers {
                out.push_str(&format!("{k}: {v}\r\n"));
            }
            out.push_str("\r\n");
            let mut bytes = out.into_bytes();
            bytes.extend_from_slice(&body);
            stream.write_all(&bytes).await.is_ok()
        }
        Reply::Stream { status, chunks } => {
            let head = format!("HTTP/1.1 {status} X\r\nConnection: close\r\n\r\n");
            let _ = stream.write_all(head.as_bytes()).await;
            for chunk in chunks {
                if stream.write_all(&chunk).await.is_err() {
                    break;
                }
                let _ = stream.flush().await;
            }
            let _ = stream.shutdown().await;
            false
        }
        Reply::Hang { raw } => {
            let _ = stream.write_all(&raw).await;
            let _ = stream.flush().await;
            std::future::pending::<()>().await;
            false
        }
        Reply::Delay(..) => unreachable!(),
    }
}

// ----------------------------------------------------------------- helpers

fn ns() -> String {
    format!("edger_{}", "ab".repeat(32))
}

fn pubkey() -> [u8; 32] {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7).wrapping_add(3);
    }
    k
}

fn record() -> SqldRegistryRecord {
    SqldRegistryRecord {
        state: SqldRegistryState::Intent,
        sql_namespace: ns(),
        public_key: pubkey(),
        quota_bytes: 16 * 1024 * 1024,
        generation: 1,
    }
}

fn fast(total_ms: u64) -> SqldAdminConfig {
    SqldAdminConfig {
        total_timeout: Duration::from_millis(total_ms),
        connect_timeout: Duration::from_secs(5),
        read_timeout: Duration::from_secs(30),
    }
}

fn client(server: &Server) -> SqldAdminClient {
    SqldAdminClient::with_config(
        &server.origin(),
        Zeroizing::new(KEY_CANARY.into()),
        fast(5_000),
    )
    .unwrap()
}

fn client_with(server: &Server, cfg: SqldAdminConfig) -> SqldAdminClient {
    SqldAdminClient::with_config(&server.origin(), Zeroizing::new(KEY_CANARY.into()), cfg).unwrap()
}

fn spki_pem(oid: &str, params: Option<Any>, bits: BitString) -> String {
    SubjectPublicKeyInfoOwned {
        algorithm: AlgorithmIdentifierOwned {
            oid: ObjectIdentifier::new_unwrap(oid),
            parameters: params,
        },
        subject_public_key: bits,
    }
    .to_pem(LineEnding::LF)
    .unwrap()
}

fn ed_pem(key: &[u8]) -> String {
    spki_pem("1.3.101.112", None, BitString::from_bytes(key).unwrap())
}

fn assert_no_canary(text: &str) {
    for canary in [KEY_CANARY, BODY_CANARY, JWT_CANARY, "127.0.0.1", "edger_"] {
        assert!(!text.contains(canary), "leaked {canary} in {text}");
    }
}

fn assert_err_clean(err: SqldAdminError) {
    assert_no_canary(&err.to_string());
    assert_no_canary(&format!("{err:?}"));
}

// ------------------------------------------------------------------ origin

#[test]
fn origin_key_and_config_validation() {
    let key = || Zeroizing::new(KEY_CANARY.to_string());
    for bad in [
        "",
        "ftp://h",
        "http://",
        "h:80",
        "http://u:p@h",
        "http://u@h",
        "http://h/x",
        "http://h/x/",
        "http://h?",
        "http://h?q=1",
        "http://h#",
        "http://h#f",
    ] {
        assert_eq!(
            SqldAdminClient::new(bad, key()).unwrap_err(),
            SqldAdminError::InvalidOrigin,
            "{bad}"
        );
    }
    for good in [
        "http://127.0.0.1:8080",
        "https://admin.example/",
        "http://[::1]:1",
    ] {
        assert!(SqldAdminClient::new(good, key()).is_ok(), "{good}");
    }
    for good in ["k", "~!", "A1b2-C3_d4.e5"] {
        assert!(
            SqldAdminClient::new("http://h", Zeroizing::new(good.into())).is_ok(),
            "{good:?}"
        );
    }
    for bad in [
        "",
        " ",
        "has space",
        "a  b",
        " lead",
        "trail ",
        "line\nbreak",
        "a\rb",
        "tab\t",
        "a\x7fb",
        "a\x01b",
        "caf\u{e9}",
    ] {
        assert_eq!(
            SqldAdminClient::new("http://h", Zeroizing::new(bad.into())).unwrap_err(),
            SqldAdminError::InvalidKey,
            "{bad:?}"
        );
    }
    let mut cfg = SqldAdminConfig::default();
    assert_eq!(cfg.total_timeout, Duration::from_secs(15));
    cfg.total_timeout = Duration::ZERO;
    assert_eq!(
        SqldAdminClient::with_config("http://h", key(), cfg).unwrap_err(),
        SqldAdminError::InvalidConfig
    );
}

#[tokio::test]
async fn config_durations_are_bounded_without_panic_or_requests() {
    let server = Server::start(|_| ok("")).await;
    let key = || Zeroizing::new(KEY_CANARY.to_string());
    let base = SqldAdminConfig::default();
    let over = MAX_CONFIG_TIMEOUT + Duration::from_nanos(1);
    for bad in [Duration::MAX, over, Duration::from_secs(61), Duration::ZERO] {
        for field in 0..3 {
            let mut cfg = base;
            match field {
                0 => cfg.total_timeout = bad,
                1 => cfg.connect_timeout = bad,
                _ => cfg.read_timeout = bad,
            }
            // Fora do `unwrap_err` do client: nenhuma chamada pode ocorrer.
            let err = SqldAdminClient::with_config(&server.origin(), key(), cfg).unwrap_err();
            assert_eq!(err, SqldAdminError::InvalidConfig, "{bad:?} field {field}");
        }
    }
    assert_eq!(server.count(), 0, "recusa antes de qualquer requisição");
    // Controle: o limite exato e valores pequenos continuam válidos e usáveis.
    let edge = SqldAdminConfig {
        total_timeout: MAX_CONFIG_TIMEOUT,
        connect_timeout: MAX_CONFIG_TIMEOUT,
        read_timeout: Duration::from_nanos(1) + Duration::from_millis(500),
    };
    let c = SqldAdminClient::with_config(&server.origin(), key(), edge).unwrap();
    assert_eq!(
        c.read_config(&ns()).await.unwrap_err(),
        SqldAdminError::InvalidResponse
    );
    assert_eq!(server.count(), 1);
    assert_eq!(MAX_CONFIG_TIMEOUT, Duration::from_secs(60));
}

#[test]
fn debug_hides_credentials_and_origin() {
    let c = SqldAdminClient::new("http://127.0.0.1:9", Zeroizing::new(KEY_CANARY.into())).unwrap();
    assert_no_canary(&format!("{c:?}"));
}

// ------------------------------------------------------------------- probe

#[tokio::test]
async fn probe_sends_no_auth_first_then_raw_basic() {
    let server = Server::start(|req| {
        if req.headers.contains_key("authorization") {
            status_body(404, "")
        } else {
            status_body(401, "")
        }
    })
    .await;
    client(&server).probe_admin_auth().await.unwrap();
    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    for r in &reqs {
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/v1/namespaces/edger_probe/config");
    }
    assert!(!reqs[0].headers.contains_key("authorization"));
    assert_eq!(
        reqs[1].headers["authorization"],
        format!("Basic {KEY_CANARY}")
    );
}

#[tokio::test]
async fn probe_refuses_open_api_before_sending_the_key() {
    for open in [200u16, 404] {
        let server = Server::start(move |_| status_body(open, "{}")).await;
        let err = client(&server).probe_admin_auth().await.unwrap_err();
        assert_eq!(err, SqldAdminError::ApiOpen, "status {open}");
        assert_eq!(server.count(), 1, "the key must not reach an open API");
        assert!(!server.requests()[0].headers.contains_key("authorization"));
        assert_err_clean(err);
    }
}

#[tokio::test]
async fn probe_with_rejected_key_is_not_ready() {
    let server = Server::start(|_| status_body(401, "")).await;
    assert_eq!(
        client(&server).probe_admin_auth().await.unwrap_err(),
        SqldAdminError::Unauthorized
    );
    assert_eq!(server.count(), 2);
    let server = Server::start(|req| {
        if req.headers.contains_key("authorization") {
            status_body(500, BODY_CANARY)
        } else {
            status_body(401, "")
        }
    })
    .await;
    let err = client(&server).probe_admin_auth().await.unwrap_err();
    assert_eq!(err, SqldAdminError::UnexpectedStatus);
    assert_err_clean(err);
}

// ------------------------------------------------------------------ create

#[tokio::test]
async fn create_posts_exact_path_and_body_once() {
    let server = Server::start(|_| ok("")).await;
    let outcome = client(&server).create_namespace(&record()).await.unwrap();
    assert_eq!(outcome, SqldCreateOutcome::Created);
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, format!("/v1/namespaces/{}/create", ns()));
    assert_eq!(r.headers["authorization"], format!("Basic {KEY_CANARY}"));
    assert_eq!(r.headers["content-type"], "application/json");
    let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    let obj = body.as_object().unwrap();
    assert_eq!(
        obj.len(),
        3,
        "exactly jwt_key, max_db_size and allow_attach"
    );
    assert_eq!(obj["jwt_key"], URL_SAFE_NO_PAD.encode(pubkey()));
    assert_eq!(obj["max_db_size"].as_u64(), Some(16 * 1024 * 1024));
    assert_eq!(obj["allow_attach"], serde_json::Value::Bool(false));
    // Sem espaços, sem padding: o componente é o texto exato.
    let text = std::str::from_utf8(&r.body).unwrap();
    assert!(text.starts_with('{') && !text.contains(' ') && !text.contains('='));
}

#[tokio::test]
async fn create_statuses_map_without_retry() {
    let cases = [
        (400u16, Ok(SqldCreateOutcome::RejectedOrExists)),
        (401, Err(SqldAdminError::Unauthorized)),
        (403, Err(SqldAdminError::UnexpectedStatus)),
        (409, Err(SqldAdminError::UnexpectedStatus)),
        (500, Err(SqldAdminError::UnexpectedStatus)),
    ];
    for (status, want) in cases {
        let server = Server::start(move |_| status_body(status, BODY_CANARY)).await;
        let got = client(&server).create_namespace(&record()).await;
        assert_eq!(got, want, "status {status}");
        assert_eq!(server.count(), 1, "no automatic retry for {status}");
        if let Err(e) = got {
            assert_err_clean(e);
        }
    }
}

#[tokio::test]
async fn create_rejects_invalid_records_without_a_request() {
    let server = Server::start(|_| ok("")).await;
    let c = client(&server);
    let mut cases: Vec<SqldRegistryRecord> = Vec::new();
    let mut r = record();
    r.state = SqldRegistryState::Conflict;
    cases.push(r);
    for quota in [0, 4096, 16 * 1024 * 1024 - 4096, 16 * 1024 * 1024 + 1] {
        let mut r = record();
        r.quota_bytes = quota;
        cases.push(r);
    }
    for bad in [
        String::new(),
        "edger_".to_string(),
        format!("edger_{}", "ab".repeat(31)),
        format!("edger_{}", "ab".repeat(33)),
        format!("edger_{}", "AB".repeat(32)),
        format!("edger_{}", "zz".repeat(32)),
        format!("other_{}", "ab".repeat(32)),
        format!("edger_{}/../x", "a".repeat(58)),
        format!("edger_{}\n", "ab".repeat(32)),
        "edger_probe".to_string(),
    ] {
        let mut r = record();
        r.sql_namespace = bad;
        cases.push(r);
    }
    for r in &cases {
        assert!(c.create_namespace(r).await.is_err());
    }
    // Controles: Intent e Proven/Ready com quota alinhada passam.
    for state in [SqldRegistryState::Proven, SqldRegistryState::Ready] {
        let mut r = record();
        r.state = state;
        r.quota_bytes = 32 * 1024 * 1024;
        assert!(c.create_namespace(&r).await.is_ok());
    }
    assert_eq!(server.count(), 2, "only the two controls reached the wire");
}

// -------------------------------------------------------------- read_config

fn config_json(jwt: &str) -> String {
    format!(r#"{{"block_reads":false,"max_db_size":"16.0 MiB","jwt_key":"{jwt}"}}"#)
}

#[tokio::test]
async fn read_config_missing_and_present_keys_over_tcp() {
    let key = pubkey();
    let component = URL_SAFE_NO_PAD.encode(key);
    let pem = ed_pem(&key).replace('\n', "\\n");
    let bodies: Vec<(String, SqldNamespaceConfig)> = vec![
        (
            config_json(&component),
            SqldNamespaceConfig::Present {
                key: SqldRemoteKey::Single(key),
            },
        ),
        (
            config_json(&pem),
            SqldNamespaceConfig::Present {
                key: SqldRemoteKey::Single(key),
            },
        ),
        (
            r#"{"jwt_key":null}"#.into(),
            SqldNamespaceConfig::Present {
                key: SqldRemoteKey::Missing,
            },
        ),
        (
            "{}".into(),
            SqldNamespaceConfig::Present {
                key: SqldRemoteKey::Missing,
            },
        ),
        (
            config_json("not-a-key"),
            SqldNamespaceConfig::Present {
                key: SqldRemoteKey::Invalid,
            },
        ),
        (
            config_json(""),
            SqldNamespaceConfig::Present {
                key: SqldRemoteKey::Invalid,
            },
        ),
    ];
    for (body, want) in bodies {
        let b = body.clone();
        let server = Server::start(move |_| ok(&b)).await;
        let got = client(&server).read_config(&ns()).await.unwrap();
        assert_eq!(got, want, "{body}");
        let r = &server.requests()[0];
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, format!("/v1/namespaces/{}/config", ns()));
        assert_eq!(r.headers["authorization"], format!("Basic {KEY_CANARY}"));
    }
    let server = Server::start(|_| status_body(404, BODY_CANARY)).await;
    assert_eq!(
        client(&server).read_config(&ns()).await.unwrap(),
        SqldNamespaceConfig::Missing
    );
}

#[tokio::test]
async fn read_config_bad_responses_are_errors_without_the_body() {
    for body in [
        format!("{{\"jwt_key\":\"{BODY_CANARY}"),
        format!("[\"{BODY_CANARY}\"]"),
        format!("{{\"jwt_key\":[\"{BODY_CANARY}\"]}}"),
        format!("{{\"jwt_key\":{{\"{BODY_CANARY}\":1}}}}"),
        "{\"jwt_key\":12}".to_string(),
        String::new(),
    ] {
        let b = body.clone();
        let server = Server::start(move |_| ok(&b)).await;
        let err = client(&server).read_config(&ns()).await.unwrap_err();
        assert_eq!(err, SqldAdminError::InvalidResponse, "{body}");
        assert_err_clean(err);
    }
    for status in [401u16, 403, 500] {
        let server = Server::start(move |_| status_body(status, JWT_CANARY)).await;
        let err = client(&server).read_config(&ns()).await.unwrap_err();
        assert_eq!(
            err,
            if status == 401 {
                SqldAdminError::Unauthorized
            } else {
                SqldAdminError::UnexpectedStatus
            }
        );
        assert_err_clean(err);
    }
}

#[tokio::test]
async fn read_config_malformed_namespace_never_reaches_the_wire() {
    let server = Server::start(|_| status_body(404, "")).await;
    let c = client(&server);
    for bad in [
        "",
        "edger_probe",
        "../config",
        "edger_/../../x",
        &format!("edger_{}%2f", "a".repeat(58)),
        &format!("edger_{}?x=1", "a".repeat(58)),
        &format!("edger_{}", "A".repeat(64)),
    ] {
        let err = c.read_config(bad).await.unwrap_err();
        assert_eq!(err, SqldAdminError::InvalidNamespace, "{bad}");
        assert_err_clean(err);
    }
    assert_eq!(server.count(), 0);
    assert_eq!(
        c.read_config(&ns()).await.unwrap(),
        SqldNamespaceConfig::Missing
    );
    assert_eq!(server.count(), 1);
}

// ------------------------------------------------------- caps and deadlines

#[tokio::test]
async fn declared_oversize_body_fails_on_the_header_alone() {
    let server = Server::start(|_| Reply::Hang {
        raw: b"HTTP/1.1 200 OK\r\nContent-Length: 70000\r\n\r\n".to_vec(),
    })
    .await;
    let c = client_with(&server, fast(4_000));
    let t = Instant::now();
    let err = c.read_config(&ns()).await.unwrap_err();
    assert_eq!(err, SqldAdminError::ResponseTooLarge);
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "must not wait for the body"
    );
}

#[tokio::test]
async fn streaming_body_without_length_is_capped_before_append() {
    let chunk = vec![b' '; 16 * 1024];
    let c2 = chunk.clone();
    let server = Server::start(move |_| Reply::Stream {
        status: 200,
        chunks: vec![c2.clone(); 5],
    })
    .await;
    assert_eq!(
        client(&server).read_config(&ns()).await.unwrap_err(),
        SqldAdminError::ResponseTooLarge
    );
    // Controle de fronteira: EXATAMENTE o teto passa pelo cap (e só falha no JSON).
    assert_eq!(MAX_ADMIN_RESPONSE_BYTES, 64 * 1024);
    let server = Server::start(move |_| Reply::Stream {
        status: 200,
        chunks: vec![chunk.clone(); 4],
    })
    .await;
    assert_eq!(
        client(&server).read_config(&ns()).await.unwrap_err(),
        SqldAdminError::InvalidResponse
    );
    // Um byte acima do teto, sem length.
    let server = Server::start(|_| Reply::Stream {
        status: 200,
        chunks: vec![vec![b' '; MAX_ADMIN_RESPONSE_BYTES], vec![b' ']],
    })
    .await;
    assert_eq!(
        client(&server).read_config(&ns()).await.unwrap_err(),
        SqldAdminError::ResponseTooLarge
    );
}

#[tokio::test]
async fn peer_that_never_finishes_respects_the_total_deadline() {
    // read_timeout (30s) >> total (400ms): só o prazo total encerra a chamada.
    let partial = Server::start(|_| Reply::Hang {
        raw: b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n{\"jwt".to_vec(),
    })
    .await;
    let silent = Server::start(|_| Reply::Hang { raw: Vec::new() }).await;
    for server in [&partial, &silent] {
        let t = Instant::now();
        let err = client_with(server, fast(400))
            .read_config(&ns())
            .await
            .unwrap_err();
        assert_eq!(err, SqldAdminError::Timeout);
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        assert_err_clean(err);
    }
    // Mesmo para o POST de CREATE: sem retry após o prazo.
    let t = Instant::now();
    let err = client_with(&silent, fast(400))
        .create_namespace(&record())
        .await
        .unwrap_err();
    assert_eq!(err, SqldAdminError::Timeout);
    assert!(t.elapsed() < Duration::from_secs(3));
    assert_eq!(
        silent.count(),
        2,
        "one request per call (read + create), no retry"
    );
}

#[tokio::test]
async fn probe_two_gets_share_one_total_budget() {
    // Cada GET (900ms) cabe no prazo total (1500ms); a soma (1800ms) não.
    let slow = |req: &Req| {
        let status = if req.headers.contains_key("authorization") {
            404
        } else {
            401
        };
        Reply::Delay(
            Duration::from_millis(900),
            Box::new(status_body(status, "")),
        )
    };
    let server = Server::start(slow).await;
    let t = Instant::now();
    let err = client_with(&server, fast(1_500))
        .probe_admin_auth()
        .await
        .unwrap_err();
    assert_eq!(err, SqldAdminError::Timeout);
    assert!(
        t.elapsed() < Duration::from_millis(2_500),
        "o segundo GET não reinicia o prazo: {:?}",
        t.elapsed()
    );
    assert_eq!(
        server.count(),
        2,
        "o 2º GET foi enviado e cortado pelo prazo"
    );
    assert_err_clean(err);
    // Controle: o mesmo servidor lento cabe num orçamento de 5s.
    client_with(&server, fast(5_000))
        .probe_admin_auth()
        .await
        .unwrap();
}

#[tokio::test]
async fn per_stage_timeouts_never_extend_the_total_deadline() {
    let silent = Server::start(|_| Reply::Hang { raw: Vec::new() }).await;
    let cfg = SqldAdminConfig {
        total_timeout: Duration::from_millis(300),
        connect_timeout: MAX_CONFIG_TIMEOUT,
        read_timeout: MAX_CONFIG_TIMEOUT,
    };
    let t = Instant::now();
    let err = client_with(&silent, cfg)
        .read_config(&ns())
        .await
        .unwrap_err();
    assert_eq!(err, SqldAdminError::Timeout);
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}

#[tokio::test]
async fn occupied_slot_wait_is_inside_the_deadline() {
    let server = Server::start(|_| Reply::Hang { raw: Vec::new() }).await;
    let a = client_with(&server, fast(600));
    let b = a.clone();
    let name = ns();
    let first = {
        let name = name.clone();
        tokio::spawn(async move { a.read_config(&name).await })
    };
    let wait_start = Instant::now();
    while server.count() == 0 {
        assert!(wait_start.elapsed() < Duration::from_secs(3));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let t = Instant::now();
    let err = b.read_config(&name).await.unwrap_err();
    // Se a espera pelo slot não tivesse prazo próprio, B esperaria o prazo do
    // titular (~600ms) e depois o seu inteiro (>1.1s).
    assert_eq!(err, SqldAdminError::Timeout);
    assert!(
        t.elapsed() < Duration::from_millis(1_000),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(first.await.unwrap().unwrap_err(), SqldAdminError::Timeout);
}

#[tokio::test]
async fn clones_share_one_slot() {
    let server =
        Server::start(|_| Reply::Delay(Duration::from_millis(150), Box::new(status_body(404, ""))))
            .await;
    let a = client(&server);
    let b = a.clone();
    let name = ns();
    let (ra, rb) = tokio::join!(a.read_config(&name), b.read_config(&name));
    assert_eq!(ra.unwrap(), SqldNamespaceConfig::Missing);
    assert_eq!(rb.unwrap(), SqldNamespaceConfig::Missing);
    assert_eq!(server.count(), 2);
    assert_eq!(server.max_inflight.load(Ordering::SeqCst), 1);
    // Controle: dois clientes independentes se sobrepõem.
    let c = client(&server);
    let (rc, rd) = tokio::join!(a.read_config(&name), c.read_config(&name));
    assert!(rc.is_ok() && rd.is_ok());
    assert_eq!(server.max_inflight.load(Ordering::SeqCst), 2);
}

// ---------------------------------------------------------------- redirects

#[tokio::test]
async fn redirects_are_errors_and_the_target_sees_nothing() {
    let target = Server::start(|_| ok("{}")).await;
    let to = format!("{}/hit", target.origin());
    for status in [301u16, 302, 307, 308] {
        let to = to.clone();
        let origin = Server::start(move |_| Reply::Full {
            status,
            headers: vec![("Location", to.clone())],
            body: Vec::new(),
        })
        .await;
        let c = client(&origin);
        assert_eq!(
            c.read_config(&ns()).await.unwrap_err(),
            SqldAdminError::Redirect
        );
        assert_eq!(
            c.create_namespace(&record()).await.unwrap_err(),
            SqldAdminError::Redirect
        );
        assert_eq!(
            c.probe_admin_auth().await.unwrap_err(),
            SqldAdminError::Redirect
        );
        assert_eq!(origin.count(), 3, "status {status}");
    }
    assert_eq!(target.count(), 0, "Location must never be followed");
}

// ----------------------------------------------------------------- no_proxy

/// Filho: roda com env de proxy apontando para o listener do pai.
#[test]
#[ignore = "child helper for no_proxy_ignores_environment_proxies"]
fn child_no_proxy_helper() {
    // Only the parent test sets these; a plain `--ignored` run has nothing to do.
    let (Ok(origin), Ok(mode)) = (
        std::env::var("SQLD_TEST_ORIGIN"),
        std::env::var("SQLD_TEST_MODE"),
    ) else {
        eprintln!("child_no_proxy_helper: not started by its parent test, nothing to run");
        return;
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        match mode.as_str() {
            "client" => {
                let c = SqldAdminClient::with_config(
                    &origin,
                    Zeroizing::new(KEY_CANARY.into()),
                    fast(5_000),
                )
                .unwrap();
                assert_eq!(
                    c.read_config(&ns()).await.unwrap(),
                    SqldNamespaceConfig::Missing
                );
            }
            "control" => {
                // reqwest padrão honra o proxy do ambiente: o resultado é irrelevante.
                let _ = reqwest::Client::builder()
                    .timeout(Duration::from_secs(3))
                    .build()
                    .unwrap()
                    .get(format!("{origin}/v1/namespaces/{}/config", ns()))
                    .send()
                    .await;
            }
            other => panic!("unknown mode {other}"),
        }
    });
}

async fn run_child(origin: &str, proxy_port: u16, mode: &str) -> bool {
    let proxy = format!("http://127.0.0.1:{proxy_port}");
    let mut cmd = tokio::process::Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "child_no_proxy_helper",
        "--ignored",
        "--test-threads=1",
    ])
    .env_clear()
    .env("SQLD_TEST_ORIGIN", origin)
    .env("SQLD_TEST_MODE", mode)
    .kill_on_drop(true);
    for var in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        cmd.env(var, &proxy);
    }
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
        Ok(status) => status.unwrap().success(),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            panic!("child timed out");
        }
    }
}

#[tokio::test]
async fn no_proxy_ignores_environment_proxies() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let proxy_task = tokio::spawn(async move {
        let mut conns = JoinSet::new();
        while let Ok((mut s, _)) = proxy.accept().await {
            h.fetch_add(1, Ordering::SeqCst);
            conns.spawn(async move {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf).await;
            });
        }
    });
    let target = Server::start(|_| status_body(404, "")).await;

    let client_ok = run_child(&target.origin(), proxy_port, "client").await;
    let (proxy_hits, target_hits) = (hits.load(Ordering::SeqCst), target.count());
    proxy_task.abort();
    assert!(client_ok, "child client must reach the target directly");
    assert_eq!(proxy_hits, 0, "SqldAdminClient must bypass the env proxy");
    assert_eq!(target_hits, 1);
}

#[tokio::test]
async fn no_proxy_control_default_reqwest_does_use_the_env_proxy() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let proxy_task = tokio::spawn(async move {
        let mut conns = JoinSet::new();
        while let Ok((mut s, _)) = proxy.accept().await {
            h.fetch_add(1, Ordering::SeqCst);
            conns.spawn(async move {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf).await;
            });
        }
    });
    let target = Server::start(|_| status_body(404, "")).await;
    run_child(&target.origin(), proxy_port, "control").await;
    let (proxy_hits, target_hits) = (hits.load(Ordering::SeqCst), target.count());
    proxy_task.abort();
    assert!(
        proxy_hits >= 1,
        "control: the env proxy is honored by default"
    );
    assert_eq!(target_hits, 0);
}

// --------------------------------------------------------- remote key parsing

#[test]
fn component_and_pem_decode_to_the_same_key() {
    let key = pubkey();
    let component = URL_SAFE_NO_PAD.encode(key);
    assert_eq!(
        parse_remote_public_key(&component),
        SqldRemoteKey::Single(key)
    );
    let pem = ed_pem(&key);
    assert_eq!(parse_remote_public_key(&pem), SqldRemoteKey::Single(key));
    // Um `\n` final é só o fim do bloco.
    assert_eq!(
        parse_remote_public_key(pem.trim_end()),
        SqldRemoteKey::Single(key)
    );
}

#[test]
fn bad_components_are_invalid() {
    let key = pubkey();
    let std_plus = STANDARD.encode([0xfbu8; 32]);
    assert!(std_plus.contains('+') || std_plus.contains('/'));
    let mut noncanonical = URL_SAFE_NO_PAD.encode([0u8; 32]);
    noncanonical.pop();
    noncanonical.push('B'); // bits finais não nulos
    let cases = [
        String::new(),
        " ".into(),
        URL_SAFE_NO_PAD.encode(&key[..31]),
        URL_SAFE_NO_PAD.encode([key.as_slice(), &[1]].concat()),
        URL_SAFE_NO_PAD.encode([] as [u8; 0]),
        base64::engine::general_purpose::URL_SAFE.encode(key), // com padding
        std_plus,
        noncanonical,
        format!(" {}", URL_SAFE_NO_PAD.encode(key)),
        format!("{}\n", URL_SAFE_NO_PAD.encode(key)),
        "A".repeat(9 * 1024),
        "####".into(),
    ];
    for c in cases {
        assert_eq!(
            parse_remote_public_key(&c),
            SqldRemoteKey::Invalid,
            "{c:.60}"
        );
    }
}

#[test]
fn bad_pems_are_invalid() {
    let key = pubkey();
    let good = ed_pem(&key);
    let ed_bits = |k: &[u8]| BitString::from_bytes(k).unwrap();
    // Chave privada PKCS#8 de verdade (ring), rótulo PRIVATE KEY.
    let pkcs8 =
        ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
    let private =
        spki::der::pem::encode_string("PRIVATE KEY", LineEnding::LF, pkcs8.as_ref()).unwrap();
    let cert_label = good.replace("PUBLIC KEY", "CERTIFICATE");
    let cases: Vec<(&str, String)> = vec![
        ("private key", private),
        ("two blocks", format!("{good}{good}")),
        ("two blocks with blank line", format!("{good}\n{good}")),
        ("trailing garbage", format!("{good}garbage\n")),
        ("leading garbage", format!("junk\n{good}")),
        ("wrong label", cert_label),
        (
            "rsa oid",
            spki_pem("1.2.840.113549.1.1.1", None, ed_bits(&key)),
        ),
        ("x25519 oid", spki_pem("1.3.101.110", None, ed_bits(&key))),
        (
            "ed25519 with NULL params",
            spki_pem("1.3.101.112", Some(Any::from(Null)), ed_bits(&key)),
        ),
        ("31 bytes", ed_pem(&key[..31])),
        ("33 bytes", ed_pem(&[key.as_slice(), &[9]].concat())),
        ("empty key", ed_pem(&[])),
        (
            "unaligned bit string",
            spki_pem(
                "1.3.101.112",
                None,
                BitString::new(1, key.to_vec()).unwrap(),
            ),
        ),
        (
            "truncated armor",
            good.replace("-----END PUBLIC KEY-----", ""),
        ),
        (
            "garbage armor",
            "-----BEGIN PUBLIC KEY-----\n!!!\n-----END PUBLIC KEY-----\n".into(),
        ),
        ("only header", "-----BEGIN".into()),
        ("oversize", format!("{good}{}", "A".repeat(9 * 1024))),
    ];
    for (name, pem) in cases {
        assert_eq!(
            parse_remote_public_key(&pem),
            SqldRemoteKey::Invalid,
            "{name}"
        );
    }
    // Controle: o mesmo construtor com os parâmetros certos é aceito.
    assert_eq!(parse_remote_public_key(&good), SqldRemoteKey::Single(key));
}

// ------------------------------------------------- real sqld (opt-in)
//
// EDGER_SQLD_BIN=/root/.local/share/edger-go-wasm/runtimes/sqld-0.24.32/bin/sqld \
//   timeout --kill-after=5 120 \
//   cargo test -p edger-orchestrator --test sqld_admin -- --ignored --nocapture
// Flags e cleanup reaproveitados de tests/sqld_provision.rs (Sqld::spawn).
// SQLD 0.24.32 `auth_middleware` faz `split_whitespace` e compara só o 2º token:
// uma chave com espaço interno nunca autentica; por isso o cliente a recusa.

#[cfg(unix)]
const SPACE_ADMIN_KEY: &str = "two words real";
#[cfg(unix)]
const PLAIN_ADMIN_KEY: &str = "NoSpaceAdminKey123";

#[cfg(unix)]
struct RealObserved {
    anonymous: u16,
    authed: Vec<(String, u16)>,
    probe: Result<(), SqldAdminError>,
    stop: String,
}

#[cfg(unix)]
/// Two DISTINCT ephemeral loopback ports for one process. Both probe
/// listeners stay open until both ports are known and only then are released:
/// two calls to a one-port helper could get the same port back from the
/// kernel, because the first probe is already closed when the second binds.
async fn real_ephemeral_port_pair() -> (u16, u16) {
    let first = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second = TcpListener::bind("127.0.0.1:0").await.unwrap();
    (
        first.local_addr().unwrap().port(),
        second.local_addr().unwrap().port(),
    )
}

/// Sobe uma instância LOCAL própria com `server_key`, observa e SEMPRE mata,
/// espera e confirma o pid ausente antes de devolver (inclusive em erro).
#[cfg(unix)]
async fn real_observe(
    bin: &str,
    server_key: &str,
    headers: &[&str],
    probe_key: Option<&str>,
) -> Result<RealObserved, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).map_err(|e| e.to_string())?;
    let pub_path = dir.path().join("ed25519-global.pub");
    std::fs::write(&pub_path, pair.public_key_pem()).map_err(|e| e.to_string())?;
    let (user_port, admin_port) = real_ephemeral_port_pair().await;
    assert_ne!(user_port, admin_port, "the two fixture ports are distinct");
    let mut child = tokio::process::Command::new(bin)
        .arg("--db-path")
        .arg(dir.path().join("data.sqld"))
        .arg("--http-listen-addr")
        .arg(format!("127.0.0.1:{user_port}"))
        .arg("--admin-listen-addr")
        .arg(format!("127.0.0.1:{admin_port}"))
        .arg("--admin-auth-key")
        .arg(server_key)
        .arg("--enable-namespaces")
        .arg("--disable-default-namespace")
        .arg("--auth-jwt-key-file")
        .arg(&pub_path)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir.path())
        .env("RUST_LOG", "error")
        .stdout(std::fs::File::create(dir.path().join("sqld-stdout.log")).unwrap())
        .stderr(std::fs::File::create(dir.path().join("sqld-stderr.log")).unwrap())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot spawn the real sqld {bin:?}: {e}"))?;
    let pid = child.id().ok_or("child pid")?;
    eprintln!(
        "space-evidence: sqld pid={pid} key_has_space={}",
        server_key.contains(' ')
    );
    let origin = format!("http://127.0.0.1:{admin_port}");
    let url = format!("{origin}/v1/namespaces/edger_probe/config");
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let observed = async {
        let deadline = Instant::now() + Duration::from_secs(30);
        let anonymous = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                return Err(format!("sqld exited during startup: {status:?}"));
            }
            match http.get(&url).send().await {
                Ok(resp) => break resp.status().as_u16(),
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(200)).await
                }
                Err(_) => return Err("the admin gate did not answer in time".to_string()),
            }
        };
        let mut authed = Vec::new();
        for h in headers {
            let resp = http
                .get(&url)
                .header("authorization", *h)
                .send()
                .await
                .map_err(|e| format!("authed GET failed: {e}"))?;
            authed.push((h.to_string(), resp.status().as_u16()));
        }
        let probe = match probe_key {
            Some(k) => {
                SqldAdminClient::new(&origin, Zeroizing::new(k.into()))
                    .map_err(|e| format!("client refused the key: {e:?}"))?
                    .probe_admin_auth()
                    .await
            }
            None => Ok(()),
        };
        Ok((anonymous, authed, probe))
    };
    let observed = tokio::time::timeout(Duration::from_secs(50), observed)
        .await
        .unwrap_or_else(|_| Err("observation timeout".to_string()));
    // shutdown BEFORE returning anything
    let kill = child.kill().await;
    let waited = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    let gone = matches!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid as i32).unwrap()),
        Err(rustix::io::Errno::SRCH)
    );
    let stop = format!("kill={kill:?} wait={waited:?} pid={pid} gone={gone}");
    eprintln!("space-evidence: {stop}");
    if !gone || !matches!(waited, Ok(Ok(_))) {
        return Err(format!("owned sqld child not reaped: {stop}"));
    }
    let (anonymous, authed, probe) = observed?;
    Ok(RealObserved {
        anonymous,
        authed,
        probe,
        stop,
    })
}

#[cfg(unix)]
fn real_bin() -> String {
    let bin = std::env::var("EDGER_SQLD_BIN").unwrap_or_else(|_| {
        panic!("EDGER_SQLD_BIN must name the real sqld binary; this test never skips")
    });
    assert!(
        std::path::Path::new(&bin).is_file(),
        "EDGER_SQLD_BIN is not a file"
    );
    bin
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires the operator-approved real sqld 0.24.32 (EDGER_SQLD_BIN)"]
async fn real_sqld_refuses_every_header_for_a_key_with_an_internal_space() {
    let bin = real_bin();
    // Negativo: nenhum header derivável da chave com espaço autentica.
    let full = format!("Basic {SPACE_ADMIN_KEY}");
    let b64 = format!("Basic {}", STANDARD.encode(SPACE_ADMIN_KEY));
    let obs = real_observe(
        &bin,
        SPACE_ADMIN_KEY,
        &[&full, "Basic two", "Basic words", "Basic real", &b64],
        None,
    )
    .await
    .expect("real sqld observation");
    eprintln!(
        "space-evidence: anonymous={} authed={:?}",
        obs.anonymous, obs.authed
    );
    assert_eq!(obs.anonymous, 401);
    for (h, status) in &obs.authed {
        assert_eq!(*status, 401, "header {h:?} must not authenticate");
    }
    // O cliente recusa a mesma chave antes de qualquer rede.
    assert_eq!(
        SqldAdminClient::new("http://127.0.0.1:9", Zeroizing::new(SPACE_ADMIN_KEY.into()))
            .unwrap_err(),
        SqldAdminError::InvalidKey
    );
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires the operator-approved real sqld 0.24.32 (EDGER_SQLD_BIN)"]
async fn real_sqld_control_key_without_space_authenticates_and_probes_ok() {
    let bin = real_bin();
    let hdr = format!("Basic {PLAIN_ADMIN_KEY}");
    let obs = real_observe(&bin, PLAIN_ADMIN_KEY, &[&hdr], Some(PLAIN_ADMIN_KEY))
        .await
        .expect("real sqld observation");
    eprintln!(
        "space-evidence: control anonymous={} authed={:?} probe={:?}",
        obs.anonymous, obs.authed, obs.probe
    );
    assert_eq!(obs.anonymous, 401);
    assert_eq!(obs.authed.len(), 1);
    assert!(matches!(obs.authed[0].1, 404 | 200), "{:?}", obs.authed);
    obs.probe.expect("client probe accepts the plain key");
    assert!(obs.stop.contains("gone=true"));
}
