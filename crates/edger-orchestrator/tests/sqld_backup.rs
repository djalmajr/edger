//! EDG-54 / 14b: `SqldBackupStore` contra um servidor TCP local real (HTTP/1.1
//! cru, NÃO é um SQLD) e o sistema de arquivos real em tempdir. O que é
//! SQLD de verdade fica para o provisionador.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use edger_orchestrator::sqld_backup::{
    BackupArtifact, RetentionStatus, SqldBackupConfig, SqldBackupError, SqldBackupStore,
    DEFAULT_MAX_BYTES, KEEP_COPIES, MAX_CONFIG_BYTES, MAX_CONFIG_TIMEOUT,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

const NS: &str = "edger_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OTHER_NS: &str = "edger_fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
const TOKEN_CANARY: &str = "CANARY-TOKEN-7f3a9c";
const BODY_CANARY: &str = "CANARY-BODY-5d21e8";

#[cfg(not(unix))]
#[test]
fn non_unix_is_unsupported_before_any_access() {
    let err = SqldBackupStore::new("http://127.0.0.1:1/", std::env::temp_dir()).unwrap_err();
    assert_eq!(err, SqldBackupError::Unsupported);
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

    // ---------------------------------------------------------------- server

    enum Step {
        Write(Vec<u8>),
        Sleep(Duration),
        Hold,
    }

    struct Server {
        origin: String,
        heads: Arc<Mutex<Vec<String>>>,
        accepted: Arc<AtomicUsize>,
        task: JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            // Aborta o loop e, com ele, o JoinSet das conexões.
            self.task.abort();
        }
    }

    impl Server {
        fn accepted(&self) -> usize {
            self.accepted.load(Ordering::SeqCst)
        }
        fn heads(&self) -> Vec<String> {
            self.heads.lock().unwrap().clone()
        }
    }

    async fn serve(steps: Vec<Step>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let heads = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(AtomicUsize::new(0));
        let steps = Arc::new(steps);
        let (h, a) = (Arc::clone(&heads), Arc::clone(&accepted));
        let task = tokio::spawn(async move {
            let mut conns = tokio::task::JoinSet::new();
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                a.fetch_add(1, Ordering::SeqCst);
                let (h, steps) = (Arc::clone(&h), Arc::clone(&steps));
                conns.spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    h.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&head).into_owned());
                    for step in steps.iter() {
                        match step {
                            Step::Write(bytes) => {
                                if sock.write_all(bytes).await.is_err() {
                                    return;
                                }
                            }
                            Step::Sleep(d) => tokio::time::sleep(*d).await,
                            Step::Hold => std::future::pending::<()>().await,
                        }
                    }
                    let _ = sock.shutdown().await;
                });
            }
        });
        Server {
            origin,
            heads,
            accepted,
            task,
        }
    }

    const CHUNKED_HEAD: &str = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
    const LAST_CHUNK: &str = "0\r\n\r\n";

    fn chunk(data: &[u8]) -> Step {
        let mut out = format!("{:x}\r\n", data.len()).into_bytes();
        out.extend_from_slice(data);
        out.extend_from_slice(b"\r\n");
        Step::Write(out)
    }

    fn w(text: &str) -> Step {
        Step::Write(text.as_bytes().to_vec())
    }

    /// 200 chunked completo com as partes dadas.
    fn dump_steps(parts: &[&[u8]]) -> Vec<Step> {
        let mut steps = vec![w(CHUNKED_HEAD)];
        steps.extend(parts.iter().map(|p| chunk(p)));
        steps.push(w(LAST_CHUNK));
        steps
    }

    // --------------------------------------------------------------- helpers

    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    fn config() -> SqldBackupConfig {
        SqldBackupConfig {
            total_timeout: Duration::from_secs(20),
            connect_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(10),
            max_bytes: 1024 * 1024,
        }
    }

    fn store(origin: &str, dir: &Path, cfg: SqldBackupConfig) -> SqldBackupStore {
        SqldBackupStore::with_config(origin, dir.to_path_buf(), cfg).unwrap()
    }

    fn tok() -> Zeroizing<String> {
        Zeroizing::new(TOKEN_CANARY.to_owned())
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// O job apaga o temporário antes de soltar o slot: espera a listagem
    /// ficar como esperada (cancelamento é assíncrono).
    async fn wait_listing(dir: &Path, expected: &[String]) {
        let start = Instant::now();
        loop {
            if listing(dir) == expected {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "listing {:?} never became {expected:?}",
                listing(dir)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn sha_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn set_mtime(path: &Path, secs: i64) {
        let ts = rustix::fs::Timespec {
            tv_sec: secs,
            tv_nsec: 0,
        };
        rustix::fs::utimensat(
            rustix::fs::CWD,
            path,
            &rustix::fs::Timestamps {
                last_access: ts,
                last_modification: ts,
            },
            rustix::fs::AtFlags::empty(),
        )
        .unwrap();
    }

    fn copy_name(ns: &str, n: u128) -> String {
        format!("{ns}-{}.sql", uuid::Uuid::from_u128(n).hyphenated())
    }

    fn seed(dir: &Path, name: &str, mtime: i64) {
        let path = dir.join(name);
        std::fs::write(&path, format!("seed {name}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        set_mtime(&path, mtime);
    }

    fn assert_static(err: SqldBackupError) {
        let text = format!("{err:?} {err}");
        for canary in [TOKEN_CANARY, BODY_CANARY, "127.0.0.1", "/dump", "Bearer"] {
            assert!(!text.contains(canary), "{canary} leaked in {text}");
        }
    }

    // ----------------------------------------------------------------- tests

    #[tokio::test]
    async fn quota_cap_refuses_an_excess_without_touching_previous_copies() {
        let dir = private_dir();
        let good = serve(dump_steps(&[b"BEGIN;\nCOMMIT;\n"])).await;
        let artifact = SqldBackupStore::new(&good.origin, dir.path().to_owned())
            .unwrap()
            .create_for_quota(NS, tok(), 128 * 1024 * 1024)
            .await
            .unwrap();
        let before = listing(dir.path());
        let over = serve(vec![
            w("HTTP/1.1 200 OK\r\nContent-Length: 536870913\r\n\r\n"),
            Step::Hold,
        ])
        .await;
        let error = SqldBackupStore::new(&over.origin, dir.path().to_owned())
            .unwrap()
            .create_for_quota(NS, tok(), 128 * 1024 * 1024)
            .await
            .unwrap_err();
        assert_eq!(error, SqldBackupError::TooLarge);
        assert_eq!(listing(dir.path()), before);
        assert_eq!(
            std::fs::read(dir.path().join(artifact.file_name)).unwrap(),
            b"BEGIN;\nCOMMIT;\n"
        );
    }

    #[tokio::test]
    async fn explicit_cap_below_the_quota_requirement_is_rejected_before_network() {
        let server = serve(dump_steps(&[b"dump"])).await;
        let dir = private_dir();
        let error = store(&server.origin, dir.path(), config())
            .create_for_quota(NS, tok(), 1024 * 1024 * 1024)
            .await
            .unwrap_err();
        assert_eq!(error, SqldBackupError::InvalidConfig);
        assert_eq!(server.accepted(), 0);
        assert!(listing(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn quota_backup_requires_a_real_terminal_commit_on_close_delimited_eof() {
        let dir = private_dir();
        let good = serve(dump_steps(&[b"BEGIN;\nCOMMIT;\n"])).await;
        SqldBackupStore::new(&good.origin, dir.path().to_owned())
            .unwrap()
            .create_for_quota(NS, tok(), 16 * 1024 * 1024)
            .await
            .unwrap();
        let before = listing(dir.path());
        for body in [
            "BEGIN;\nINSERT INTO t VALUES ('payload\nCOMMIT;\n",
            "BEGIN;\n-- COMMIT;\n",
            "BEGIN;\n/* COMMIT; */\n",
        ] {
            let server = serve(vec![
                w("HTTP/1.0 200 OK\r\nConnection: close\r\n\r\n"),
                w(body),
            ])
            .await;
            let error = SqldBackupStore::new(&server.origin, dir.path().to_owned())
                .unwrap()
                .create_for_quota(NS, tok(), 16 * 1024 * 1024)
                .await
                .unwrap_err();
            assert_eq!(error, SqldBackupError::Incomplete);
            assert_eq!(listing(dir.path()), before);
            assert!(server.heads()[0].starts_with("GET /dump HTTP/1.0\r\n"));
        }
    }

    #[tokio::test]
    async fn publishes_chunked_dump_privately_with_real_bytes_and_digest() {
        let dump = format!("BEGIN;\nINSERT INTO t VALUES('{BODY_CANARY}');\nCOMMIT;\n");
        let parts: Vec<&[u8]> = vec![
            &dump.as_bytes()[..7],
            &dump.as_bytes()[7..20],
            &dump.as_bytes()[20..],
        ];
        let server = serve(dump_steps(&parts)).await;
        let dir = private_dir();
        let art = store(&server.origin, dir.path(), config())
            .create(NS, tok())
            .await
            .unwrap();

        assert_eq!(art.retention, RetentionStatus::Complete);
        assert_eq!(art.bytes, dump.len() as u64);
        assert_eq!(art.sha256, sha_hex(dump.as_bytes()));
        assert_eq!(listing(dir.path()), vec![art.file_name.clone()]);
        assert!(!art.file_name.contains('/'));
        let (stem, ext) = art.file_name.rsplit_once('.').unwrap();
        assert_eq!(ext, "sql");
        let (ns, uuid) = stem.split_at(NS.len());
        assert_eq!(ns, NS);
        assert!(uuid::Uuid::parse_str(uuid.strip_prefix('-').unwrap()).is_ok());
        let path = dir.path().join(&art.file_name);
        assert_eq!(std::fs::read(&path).unwrap(), dump.as_bytes());
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert!(meta.is_file());
        assert_eq!(meta.permissions().mode() & 0o7777, 0o600);
        assert_eq!(meta.nlink(), 1);
        // O caminho/segredo nunca entram no metadado.
        let dbg = format!("{art:?}");
        assert!(!dbg.contains(TOKEN_CANARY) && !dbg.contains(BODY_CANARY));
        assert!(!dbg.contains(dir.path().to_str().unwrap()));

        // Pedido: GET /dump sem query, Bearer do app e namespace no_pad.
        let heads = server.heads();
        assert_eq!(heads.len(), 1);
        let head = heads[0].to_ascii_lowercase();
        assert!(head.starts_with("get /dump http/1.1\r\n"), "{head}");
        assert!(head.contains(&format!(
            "authorization: bearer {}",
            TOKEN_CANARY.to_lowercase()
        )));
        use base64::Engine as _;
        let ns_b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(NS);
        assert!(head.contains(&format!("x-namespace-bin: {}", ns_b64.to_lowercase())));
    }

    #[tokio::test]
    async fn dump_is_restorable_into_sqlite_inside_the_tempdir() {
        // Fixture de dump em forma SQL (NÃO é saída de SQLD real): >64 KiB
        // para cruzar vários chunks, com UTF-8 multibyte.
        let mut sql = String::from("PRAGMA foreign_keys=OFF;\nBEGIN TRANSACTION;\n");
        sql.push_str("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);\n");
        for i in 0..2000 {
            sql.push_str(&format!(
                "INSERT INTO notes VALUES({i},'ação-{i}-{}');\n",
                "x".repeat(40)
            ));
        }
        sql.push_str("COMMIT;\n");
        assert!(sql.len() > 64 * 1024);
        let parts: Vec<&[u8]> = sql.as_bytes().chunks(10_007).collect();
        let server = serve(dump_steps(&parts)).await;
        let dir = private_dir();
        let art = store(&server.origin, dir.path(), config())
            .create(NS, tok())
            .await
            .unwrap();
        let bytes = std::fs::read(dir.path().join(&art.file_name)).unwrap();
        assert_eq!(art.sha256, sha_hex(&bytes));
        assert_eq!(bytes, sql.as_bytes());

        let scratch = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(scratch.path().join("restore.db")).unwrap();
        db.execute_batch(std::str::from_utf8(&bytes).unwrap())
            .unwrap();
        let (count, last): (i64, String) = db
            .query_row("SELECT count(*), max(body) FROM notes", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(count, 2000);
        assert!(last.starts_with("ação-"));
    }

    #[tokio::test]
    async fn cap_is_exact_across_chunks_and_one_more_byte_is_refused() {
        let cfg = SqldBackupConfig {
            max_bytes: 10,
            ..config()
        };
        let dir = private_dir();
        let exact = serve(dump_steps(&[b"01234", b"567", b"89"])).await;
        let art = store(&exact.origin, dir.path(), cfg)
            .create(NS, tok())
            .await
            .unwrap();
        assert_eq!(art.bytes, 10);

        let over = serve(dump_steps(&[b"01234", b"567", b"89A"])).await;
        let before = listing(dir.path());
        let err = store(&over.origin, dir.path(), cfg)
            .create(NS, tok())
            .await
            .unwrap_err();
        assert_eq!(err, SqldBackupError::TooLarge);
        assert_eq!(listing(dir.path()), before, "no partial, previous intact");
    }

    #[tokio::test]
    async fn content_length_above_cap_is_refused_before_any_body_is_read() {
        let steps = vec![
            w("HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\n"),
            Step::Hold,
        ];
        let server = serve(steps).await;
        let dir = private_dir();
        let started = Instant::now();
        let err = store(&server.origin, dir.path(), config())
            .create(NS, tok())
            .await
            .unwrap_err();
        assert_eq!(err, SqldBackupError::TooLarge);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "must not wait for the body"
        );
        assert!(listing(dir.path()).is_empty(), "no file was even created");
    }

    #[tokio::test]
    async fn oversized_stream_without_content_length_is_refused_midway() {
        let big = vec![b'x'; 4096];
        let mut steps = vec![w(CHUNKED_HEAD)];
        for _ in 0..8 {
            steps.push(chunk(&big));
        }
        steps.push(Step::Hold);
        let server = serve(steps).await;
        let dir = private_dir();
        let cfg = SqldBackupConfig {
            max_bytes: 10_000,
            ..config()
        };
        let err = store(&server.origin, dir.path(), cfg)
            .create(NS, tok())
            .await
            .unwrap_err();
        assert_eq!(err, SqldBackupError::TooLarge);
        wait_listing(dir.path(), &[]).await;
    }

    #[tokio::test]
    async fn truncated_bodies_never_publish_and_keep_the_previous_backup() {
        let ok = serve(dump_steps(&[b"GOOD DUMP"])).await;
        let dir = private_dir();
        let first = store(&ok.origin, dir.path(), config())
            .create(NS, tok())
            .await
            .unwrap();
        let expected = vec![first.file_name.clone()];

        let cases: Vec<(&str, Vec<Step>)> = vec![
            (
                "content-length short",
                vec![
                    w("HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n"),
                    w("0123456789"),
                ],
            ),
            (
                "chunked without terminator",
                vec![w(CHUNKED_HEAD), chunk(b"partial dump")],
            ),
            (
                "chunk cut inside its own size",
                vec![w(CHUNKED_HEAD), w("20\r\nshort")],
            ),
        ];
        for (name, steps) in cases {
            let server = serve(steps).await;
            let err = store(&server.origin, dir.path(), config())
                .create(NS, tok())
                .await
                .unwrap_err();
            assert_eq!(err, SqldBackupError::Incomplete, "{name}");
            wait_listing(dir.path(), &expected).await;
            let kept = std::fs::read(dir.path().join(&first.file_name)).unwrap();
            assert_eq!(kept, b"GOOD DUMP", "{name}: previous backup preserved");
        }
    }

    #[tokio::test]
    async fn status_and_response_shape_table() {
        let json_error = format!("{{\"error\":\"{BODY_CANARY} {TOKEN_CANARY}\"}}");
        let cases: Vec<(String, SqldBackupError)> = vec![
            (
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".into(),
                SqldBackupError::Unauthorized,
            ),
            (
                format!("HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\n\r\n{json_error}", json_error.len()),
                SqldBackupError::Unauthorized,
            ),
            (
                format!("HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{json_error}", json_error.len()),
                SqldBackupError::UnexpectedStatus,
            ),
            (
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n".into(),
                SqldBackupError::UnexpectedStatus,
            ),
            (
                "HTTP/1.1 204 No Content\r\n\r\n".into(),
                SqldBackupError::UnexpectedStatus,
            ),
            (
                "HTTP/1.1 206 Partial Content\r\nContent-Length: 4\r\n\r\nPART".into(),
                SqldBackupError::UnexpectedStatus,
            ),
            // 200 que NÃO é dump: erro JSON, corpo codificado, corpo vazio.
            (
                format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{json_error}", json_error.len()),
                SqldBackupError::InvalidResponse,
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Type: Application/JSON; charset=utf-8\r\nContent-Length: 2\r\n\r\n{}".into(),
                SqldBackupError::InvalidResponse,
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Type: application/problem+json\r\nContent-Length: 2\r\n\r\n{}".into(),
                SqldBackupError::InvalidResponse,
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 4\r\n\r\nGZIP".into(),
                SqldBackupError::InvalidResponse,
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".into(),
                SqldBackupError::InvalidResponse,
            ),
        ];
        for (raw, expected) in cases {
            let server = serve(vec![w(&raw)]).await;
            let dir = private_dir();
            let err = store(&server.origin, dir.path(), config())
                .create(NS, tok())
                .await
                .unwrap_err();
            assert_eq!(err, expected, "{raw}");
            assert_static(err);
            assert!(listing(dir.path()).is_empty(), "{raw}");
        }
    }

    #[tokio::test]
    async fn redirect_is_refused_and_the_target_receives_zero_requests() {
        let target = serve(dump_steps(&[b"MUST NOT BE FETCHED"])).await;
        for code in [301, 302, 303, 307, 308] {
            let location = format!("{}dump", target.origin);
            let raw = format!(
                "HTTP/1.1 {code} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
            );
            let server = serve(vec![w(&raw)]).await;
            let dir = private_dir();
            let err = store(&server.origin, dir.path(), config())
                .create(NS, tok())
                .await
                .unwrap_err();
            assert_eq!(err, SqldBackupError::Redirect, "{code}");
            assert!(listing(dir.path()).is_empty());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(target.accepted(), 0, "redirect destination was contacted");
    }

    #[tokio::test]
    async fn a_body_that_never_ends_hits_the_read_timeout() {
        let server = serve(vec![w(CHUNKED_HEAD), chunk(b"first"), Step::Hold]).await;
        let dir = private_dir();
        let cfg = SqldBackupConfig {
            read_timeout: Duration::from_millis(300),
            ..config()
        };
        let started = Instant::now();
        let err = store(&server.origin, dir.path(), cfg)
            .create(NS, tok())
            .await
            .unwrap_err();
        assert_eq!(err, SqldBackupError::Timeout);
        assert!(started.elapsed() < Duration::from_secs(5));
        wait_listing(dir.path(), &[]).await;
    }

    #[tokio::test]
    async fn the_total_deadline_covers_a_trickling_body_that_resets_read_timeout() {
        // Um chunk a cada 100 ms nunca estoura a leitura (5 s) mas o prazo
        // TOTAL de 600 ms vale para a operação inteira.
        let mut steps = vec![w(CHUNKED_HEAD)];
        for _ in 0..100 {
            steps.push(chunk(b"tick"));
            steps.push(Step::Sleep(Duration::from_millis(100)));
        }
        let server = serve(steps).await;
        let dir = private_dir();
        let cfg = SqldBackupConfig {
            total_timeout: Duration::from_millis(600),
            read_timeout: Duration::from_secs(5),
            ..config()
        };
        let started = Instant::now();
        let err = store(&server.origin, dir.path(), cfg)
            .create(NS, tok())
            .await
            .unwrap_err();
        assert_eq!(err, SqldBackupError::Timeout);
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
        wait_listing(dir.path(), &[]).await;
    }

    #[tokio::test]
    async fn a_busy_slot_times_out_at_admission_without_any_request() {
        let slow = serve(vec![w(CHUNKED_HEAD), chunk(b"first"), Step::Hold]).await;
        let dir = private_dir();
        let a = store(&slow.origin, dir.path(), config());
        let b = a.clone(); // mesmo slot
        let first = tokio::spawn(async move { a.create(NS, tok()).await });
        // espera a primeira operação ocupar o slot e chegar ao servidor
        let start = Instant::now();
        while slow.accepted() == 0 {
            assert!(start.elapsed() < Duration::from_secs(5));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        // O clone compartilha o slot e fica na admissão (o AdmissionTimeout
        // com prazo curto é provado no teste unitário do job bloqueado).
        let second = tokio::time::timeout(Duration::from_millis(500), b.create(OTHER_NS, tok()));
        assert!(second.await.is_err(), "second call must wait at admission");
        assert_eq!(
            slow.accepted(),
            1,
            "no second connection while the slot is held"
        );

        // Cancela a primeira: o job termina o write, apaga o temporário e
        // libera o slot (a espera por listagem vazia prova a ordem).
        first.abort();
        let _ = first.await;
        wait_listing(dir.path(), &[]).await;
    }

    #[tokio::test]
    async fn caller_cancellation_leaves_nothing_and_keeps_the_previous_backup() {
        let ok = serve(dump_steps(&[b"PREVIOUS"])).await;
        let dir = private_dir();
        let first = store(&ok.origin, dir.path(), config())
            .create(NS, tok())
            .await
            .unwrap();
        let expected = vec![first.file_name.clone()];

        let slow = serve(vec![
            w(CHUNKED_HEAD),
            chunk(b"first"),
            chunk(b"second"),
            Step::Hold,
        ])
        .await;
        let s = store(&slow.origin, dir.path(), config());
        let task = tokio::spawn(async move { s.create(NS, tok()).await });
        let start = Instant::now();
        // espera o temporário aparecer (job iniciado pelo 1º chunk)
        while listing(dir.path()).len() < 2 {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "staging never appeared"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        wait_listing(dir.path(), &expected).await;
        assert_eq!(
            std::fs::read(dir.path().join(&first.file_name)).unwrap(),
            b"PREVIOUS"
        );
    }

    #[tokio::test]
    async fn token_and_body_canaries_never_reach_errors_or_debug() {
        let server = serve(dump_steps(&[BODY_CANARY.as_bytes()])).await;
        let dir = private_dir();
        let s = store(&server.origin, dir.path(), config());
        for text in [
            format!("{s:?}"),
            format!("{:?}", SqldBackupConfig::default()),
        ] {
            assert!(!text.contains(TOKEN_CANARY) && !text.contains(dir.path().to_str().unwrap()));
        }
        // Dump ok com o canário no CORPO: só o arquivo o contém.
        let art = s.create(NS, tok()).await.unwrap();
        let art_dbg = format!("{art:?}");
        assert!(!art_dbg.contains(BODY_CANARY) && !art_dbg.contains(TOKEN_CANARY));

        // Cada variante de erro tem texto estático.
        for err in [
            SqldBackupError::Unsupported,
            SqldBackupError::InvalidOrigin,
            SqldBackupError::InvalidConfig,
            SqldBackupError::InvalidDirectory,
            SqldBackupError::InvalidNamespace,
            SqldBackupError::InvalidToken,
            SqldBackupError::Client,
            SqldBackupError::AdmissionTimeout,
            SqldBackupError::Timeout,
            SqldBackupError::Transport,
            SqldBackupError::Redirect,
            SqldBackupError::Unauthorized,
            SqldBackupError::UnexpectedStatus,
            SqldBackupError::InvalidResponse,
            SqldBackupError::TooLarge,
            SqldBackupError::Incomplete,
            SqldBackupError::Fs,
            SqldBackupError::Cancelled,
        ] {
            assert_static(err);
        }
        // Erro de transporte real (porta fechada) com token no pedido.
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", closed.local_addr().unwrap());
        drop(closed);
        let err = store(&origin, dir.path(), config())
            .create(NS, tok())
            .await
            .unwrap_err();
        assert_eq!(err, SqldBackupError::Transport);
        assert_static(err);
    }

    #[tokio::test]
    async fn hostile_origins_namespaces_tokens_and_configs_are_refused() {
        let dir = private_dir();
        let origins = [
            "https://sqld.local:8080/",
            "ftp://sqld.local/",
            "http://sqld.local:9090/",
            "http://sqld.local:9090",
            "http://user@sqld.local:8080/",
            "http://user:pw@sqld.local:8080/",
            "http://sqld.local:8080/?x=1",
            "http://sqld.local:8080/#frag",
            "http://sqld.local:8080/dump",
            "http://sqld.local:8080/v1/",
            "http://",
            "sqld.local:8080",
            "",
            "not a url",
        ];
        for origin in origins {
            let err = SqldBackupStore::new(origin, dir.path().to_path_buf()).unwrap_err();
            assert_eq!(err, SqldBackupError::InvalidOrigin, "{origin}");
        }
        for ok in [
            "http://sqld.local:8080/",
            "http://sqld.local/",
            "http://127.0.0.1:8080",
        ] {
            assert!(
                SqldBackupStore::new(ok, dir.path().to_path_buf()).is_ok(),
                "{ok}"
            );
        }

        let server = serve(dump_steps(&[b"never"])).await;
        let s = store(&server.origin, dir.path(), config());
        let long = format!("edger_{}", "a".repeat(65));
        let namespaces = [
            "",
            "edger_",
            "edger_probe",
            "ns",
            &NS[..NS.len() - 1],
            &long,
            &NS.to_uppercase(),
            &NS.replace("edger_", "EDGER_"),
            &NS.replace("0123", "ghij"),
            &format!("{NS}\n"),
            &format!("{NS}/../x"),
            &format!("../{NS}"),
            &format!("{NS}.evil.example"),
            &format!("{NS}:80"),
            "edger_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdé",
        ];
        for ns in namespaces {
            let err = s.create(ns, tok()).await.unwrap_err();
            assert_eq!(err, SqldBackupError::InvalidNamespace, "{ns:?}");
        }
        let tokens = [
            "",
            " ",
            "a b",
            "tok\n",
            "tok\r\nX-Evil: 1",
            "tok\0",
            "tok\u{7f}",
            "tókén",
            "\ttok",
        ];
        for token in tokens {
            let err = s
                .create(NS, Zeroizing::new(token.to_owned()))
                .await
                .unwrap_err();
            assert_eq!(err, SqldBackupError::InvalidToken, "{token:?}");
        }
        assert_eq!(
            server.accepted(),
            0,
            "invalid inputs never reach the network"
        );
        assert!(listing(dir.path()).is_empty());

        let base = config();
        let bad = [
            SqldBackupConfig {
                total_timeout: Duration::ZERO,
                ..base
            },
            SqldBackupConfig {
                connect_timeout: Duration::ZERO,
                ..base
            },
            SqldBackupConfig {
                read_timeout: Duration::ZERO,
                ..base
            },
            SqldBackupConfig {
                total_timeout: Duration::MAX,
                ..base
            },
            SqldBackupConfig {
                connect_timeout: Duration::MAX,
                ..base
            },
            SqldBackupConfig {
                read_timeout: Duration::MAX,
                ..base
            },
            SqldBackupConfig {
                total_timeout: MAX_CONFIG_TIMEOUT + Duration::from_nanos(1),
                ..base
            },
            SqldBackupConfig {
                read_timeout: MAX_CONFIG_TIMEOUT + Duration::from_secs(1),
                ..base
            },
            SqldBackupConfig {
                max_bytes: 0,
                ..base
            },
            SqldBackupConfig {
                max_bytes: MAX_CONFIG_BYTES + 1,
                ..base
            },
            SqldBackupConfig {
                max_bytes: u64::MAX,
                ..base
            },
        ];
        for cfg in bad {
            let err = SqldBackupStore::with_config(&server.origin, dir.path().to_path_buf(), cfg)
                .unwrap_err();
            assert_eq!(err, SqldBackupError::InvalidConfig, "{cfg:?}");
        }
        let edge = SqldBackupConfig {
            total_timeout: MAX_CONFIG_TIMEOUT,
            connect_timeout: MAX_CONFIG_TIMEOUT,
            read_timeout: MAX_CONFIG_TIMEOUT,
            max_bytes: MAX_CONFIG_BYTES,
        };
        assert!(
            SqldBackupStore::with_config(&server.origin, dir.path().to_path_buf(), edge).is_ok()
        );
        let d = SqldBackupConfig::default();
        assert_eq!(d.total_timeout, Duration::from_secs(1800));
        assert_eq!(d.connect_timeout, Duration::from_secs(5));
        assert_eq!(d.read_timeout, Duration::from_secs(30));
        assert_eq!(d.max_bytes, DEFAULT_MAX_BYTES);
        assert_eq!(DEFAULT_MAX_BYTES, 256 * 1024 * 1024);
        assert_eq!(KEEP_COPIES, 3);
    }

    #[tokio::test]
    async fn private_directory_boundary_is_checked_without_creating_or_chmoding() {
        let parent = tempfile::tempdir().unwrap();
        std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let parent_mode = || {
            std::fs::metadata(parent.path())
                .unwrap()
                .permissions()
                .mode()
        };
        let before = parent_mode();
        let new = |p: &Path| SqldBackupStore::new("http://127.0.0.1:1/", p.to_path_buf());

        // inexistente: recusa e NÃO cria
        let missing = parent.path().join("missing");
        assert_eq!(
            new(&missing).unwrap_err(),
            SqldBackupError::InvalidDirectory
        );
        assert!(!missing.exists());
        // modos diferentes de 0700 (mais abertos E mais fechados)
        for mode in [
            0o755, 0o750, 0o770, 0o701, 0o777, 0o500, 0o600, 0o1700, 0o2700,
        ] {
            let d = parent.path().join(format!("m{mode:o}"));
            std::fs::create_dir(&d).unwrap();
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(
                new(&d).unwrap_err(),
                SqldBackupError::InvalidDirectory,
                "{mode:o}"
            );
            assert_eq!(
                std::fs::metadata(&d).unwrap().permissions().mode() & 0o7777,
                mode,
                "no chmod"
            );
        }
        // symlink para um diretório 0700 válido
        let real = parent.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = parent.path().join("link");
        symlink(&real, &link).unwrap();
        assert_eq!(new(&link).unwrap_err(), SqldBackupError::InvalidDirectory);
        assert!(new(&real).is_ok());
        // arquivo comum, caminho relativo, vazio
        let file = parent.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(new(&file).unwrap_err(), SqldBackupError::InvalidDirectory);
        assert_eq!(
            new(Path::new("relative-dir")).unwrap_err(),
            SqldBackupError::InvalidDirectory
        );
        assert_eq!(
            new(Path::new("")).unwrap_err(),
            SqldBackupError::InvalidDirectory
        );
        // dono diferente do euid (só provável como root)
        if rustix::process::geteuid().is_root() {
            let foreign = parent.path().join("foreign");
            std::fs::create_dir(&foreign).unwrap();
            std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::os::unix::fs::chown(&foreign, Some(12345), Some(12345)).unwrap();
            assert_eq!(
                new(&foreign).unwrap_err(),
                SqldBackupError::InvalidDirectory
            );
        }
        assert_eq!(parent_mode(), before, "existing parent never chmoded");
    }

    #[tokio::test]
    async fn directory_that_degrades_after_construction_is_refused_at_write() {
        let server = serve(dump_steps(&[b"data"])).await;
        let dir = private_dir();
        let s = store(&server.origin, dir.path(), config());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o750)).unwrap();
        let err = s.create(NS, tok()).await.unwrap_err();
        assert_eq!(err, SqldBackupError::InvalidDirectory);
        assert!(listing(dir.path()).is_empty());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        s.create(NS, tok()).await.unwrap();
    }

    async fn make_copy(dir: &Path, ns: &str, body: &str) -> BackupArtifact {
        let server = serve(dump_steps(&[body.as_bytes()])).await;
        store(&server.origin, dir, config())
            .create(ns, tok())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn retention_keeps_exactly_three_and_orders_by_host_mtime() {
        let dir = private_dir();
        let base = 1_700_000_000i64;
        let mut made: Vec<String> = Vec::new();
        for i in 0..6i64 {
            let art = make_copy(dir.path(), NS, &format!("copy {i}")).await;
            assert_eq!(art.retention, RetentionStatus::Complete);
            made.push(art.file_name.clone());
            // O mtime do host decide a ordem (não o nome/uuid aleatório).
            set_mtime(&dir.path().join(&art.file_name), base + i * 10);
            let mut expect: Vec<String> = made.iter().rev().take(KEEP_COPIES).cloned().collect();
            expect.sort();
            assert_eq!(listing(dir.path()), expect, "after copy {i}");
        }
        assert_eq!(listing(dir.path()).len(), 3);
        // Os bytes das sobreviventes são os reais.
        for name in &made[3..] {
            assert!(std::fs::read_to_string(dir.path().join(name))
                .unwrap()
                .starts_with("copy "));
        }
    }

    #[tokio::test]
    async fn retention_ties_break_by_name_and_new_copy_is_always_kept() {
        let dir = private_dir();
        // 4 anteriores com o MESMO mtime (no futuro): empate por nome.
        let future = 4_000_000_000i64;
        for n in 1..=4u128 {
            seed(dir.path(), &copy_name(NS, n), future);
        }
        let art = make_copy(dir.path(), NS, "new").await;
        let mut expect = vec![copy_name(NS, 3), copy_name(NS, 4), art.file_name.clone()];
        expect.sort();
        assert_eq!(listing(dir.path()), expect);
        assert_eq!(art.retention, RetentionStatus::Complete);
        // A nova (mtime = agora, MAIS ANTIGO que o futuro) nunca é removida.
        assert!(dir.path().join(&art.file_name).exists());
    }

    #[tokio::test]
    async fn retention_never_touches_other_namespaces_or_unrecognised_entries() {
        let dir = private_dir();
        let old = 1_000_000_000i64;
        // Outro namespace: 5 cópias válidas, todas preservadas.
        let others: Vec<String> = (1..=5u128).map(|n| copy_name(OTHER_NS, n)).collect();
        for (i, name) in others.iter().enumerate() {
            seed(dir.path(), name, old + i as i64);
        }
        // Entradas não geradas / inelegíveis com nome de namespace próprio.
        seed(dir.path(), "notes.txt", old);
        seed(dir.path(), &format!("{NS}-notauuid.sql"), old);
        seed(dir.path(), &format!("{}.bak", copy_name(NS, 90)), old);
        seed(dir.path(), &copy_name(NS, 91).to_uppercase(), old);
        seed(dir.path(), ".tmp-stale", old);
        // diretório com nome válido
        let as_dir = copy_name(NS, 92);
        std::fs::create_dir(dir.path().join(&as_dir)).unwrap();
        // modo 0600 como um arquivo: só o TIPO o torna inelegível.
        std::fs::set_permissions(
            dir.path().join(&as_dir),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        // symlink com nome válido apontando para fora
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("precious");
        std::fs::write(&target, b"precious").unwrap();
        // 0600/euid/nlink 1: seguir o symlink o tornaria elegível.
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link_name = copy_name(NS, 93);
        symlink(&target, dir.path().join(&link_name)).unwrap();
        // modo errado e hardlink (nlink 2)
        let wide = copy_name(NS, 94);
        seed(dir.path(), &wide, old);
        std::fs::set_permissions(
            dir.path().join(&wide),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let linked = copy_name(NS, 95);
        seed(dir.path(), &linked, old);
        std::fs::hard_link(dir.path().join(&linked), outside.path().join("alias")).unwrap();
        // dono diferente do euid (0600, nlink 1): inelegível (só como root)
        let foreign_owner = copy_name(NS, 96);
        let is_root = rustix::process::geteuid().is_root();
        if is_root {
            seed(dir.path(), &foreign_owner, old);
            std::os::unix::fs::chown(dir.path().join(&foreign_owner), Some(12345), Some(12345))
                .unwrap();
        }
        // 3 elegíveis antigas do próprio namespace
        for n in 1..=3u128 {
            seed(dir.path(), &copy_name(NS, n), old + n as i64);
        }
        let before: std::collections::BTreeSet<String> = listing(dir.path()).into_iter().collect();

        let art = make_copy(dir.path(), NS, "fresh").await;
        assert_eq!(art.retention, RetentionStatus::Complete);

        let after: std::collections::BTreeSet<String> = listing(dir.path()).into_iter().collect();
        // Só a MAIS ANTIGA elegível do próprio namespace saiu; a nova entrou.
        let removed: Vec<_> = before.difference(&after).cloned().collect();
        assert_eq!(removed, vec![copy_name(NS, 1)]);
        let added: Vec<_> = after.difference(&before).cloned().collect();
        assert_eq!(added, vec![art.file_name.clone()]);
        // Nada do resto foi tocado.
        assert_eq!(std::fs::read(&target).unwrap(), b"precious");
        assert!(std::fs::symlink_metadata(dir.path().join(&link_name))
            .unwrap()
            .is_symlink());
        assert!(dir.path().join(&as_dir).is_dir());
        assert!(!is_root || dir.path().join(&foreign_owner).exists());
        assert_eq!(
            std::fs::metadata(dir.path().join(&wide))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o644
        );
        for name in &others {
            assert!(dir.path().join(name).exists(), "{name}");
        }
    }

    #[tokio::test]
    async fn failed_creates_never_run_retention() {
        let dir = private_dir();
        for n in 1..=3u128 {
            seed(dir.path(), &copy_name(NS, n), 1_000_000_000 + n as i64);
        }
        let before = listing(dir.path());
        let server = serve(vec![w(CHUNKED_HEAD), chunk(b"partial")]).await;
        let err = store(&server.origin, dir.path(), config())
            .create(NS, tok())
            .await
            .unwrap_err();
        assert_eq!(err, SqldBackupError::Incomplete);
        assert_eq!(
            listing(dir.path()),
            before,
            "no copy removed by a failed run"
        );
    }

    #[test]
    fn capability_probe_for_setpriv_runs() {
        // Sob `setpriv --bounding-set=-dac_override,-dac_read_search` o
        // gate exporta a variável: a suíte de modos roda SEM bypass de DAC.
        if std::env::var_os("EDGER_BACKUP_EXPECT_NO_DAC").is_none() {
            return;
        }
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let cap_eff = status
            .lines()
            .find_map(|l| l.strip_prefix("CapEff:"))
            .map(|v| u64::from_str_radix(v.trim(), 16).unwrap())
            .unwrap();
        assert_eq!(cap_eff & (1 << 1), 0, "CAP_DAC_OVERRIDE must be dropped");
        assert_eq!(cap_eff & (1 << 2), 0, "CAP_DAC_READ_SEARCH must be dropped");
    }

    #[tokio::test]
    async fn environment_proxies_are_ignored() {
        if let Ok(origin) = std::env::var("EDGER_BACKUP_PROXY_CHILD_ORIGIN") {
            // Filho: proxies de ambiente apontam para o contador do pai.
            let dir = private_dir();
            let art = store(&origin, dir.path(), config())
                .create(NS, tok())
                .await
                .unwrap();
            assert_eq!(art.bytes, 9);
            return;
        }
        let proxy = serve(vec![w(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n",
        )])
        .await;
        let origin = serve(dump_steps(&[b"via-proxy"])).await;
        let proxy_url = proxy.origin.trim_end_matches('/').to_owned();
        let exe = std::env::current_exe().unwrap();
        let origin_url = origin.origin.clone();
        let status = tokio::task::spawn_blocking(move || {
            std::process::Command::new(exe)
                .args([
                    "--exact",
                    "unix::environment_proxies_are_ignored",
                    "--test-threads=1",
                ])
                .env("EDGER_BACKUP_PROXY_CHILD_ORIGIN", origin_url)
                .env("HTTP_PROXY", &proxy_url)
                .env("http_proxy", &proxy_url)
                .env("ALL_PROXY", &proxy_url)
                .env("all_proxy", &proxy_url)
                .env_remove("NO_PROXY")
                .env_remove("no_proxy")
                .status()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(
            status.success(),
            "child create through env proxies must succeed direct"
        );
        assert_eq!(proxy.accepted(), 0, "the env proxy received a connection");
        assert_eq!(origin.accepted(), 1);
    }
}
