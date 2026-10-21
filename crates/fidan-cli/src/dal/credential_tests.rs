//! Credential tests use isolated test processes and an in-memory keychain only.
use clap::Parser;
use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

const TOKEN_A: &str = "dal_fixture_registry_a";
const TOKEN_B: &str = "dal_fixture_registry_b";
const ENV_TOKEN: &str = "dal_explicit_fixture_override";
struct Registry {
    address: String,
    headers: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Registry {
    fn new() -> Self {
        Self::response(None)
    }
    fn response(redirect: Option<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let headers = Arc::new(Mutex::new(Vec::new()));
        let seen = headers.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            for incoming in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                let mut stream = incoming.unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut request = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while request.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                    if line.to_ascii_lowercase().starts_with("authorization:") {
                        seen.lock().unwrap().push(line.trim().to_owned());
                    }
                    line.clear();
                }
                let body =
                    r#"{"username":"fixture","email":"fixture@example.test","display_name":null}"#;
                if let Some(location) = &redirect {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    continue;
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self {
            address: format!("http://{address}"),
            headers,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Registry {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address.trim_start_matches("http://"));
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
fn command(args: &[&str]) -> anyhow::Result<()> {
    let parsed = crate::Cli::try_parse_from(std::iter::once("fidan").chain(args.iter().copied()))?;
    let crate::Command::Dal { command } = parsed.command else {
        panic!("expected Dal command")
    };
    super::run(command)
}

#[test]
fn registry_scoped_credentials() {
    if let Ok(case) = std::env::var("FIDAN_TEST_CREDENTIAL_CASE") {
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
        let a = std::env::var("FIDAN_TEST_REGISTRY_A").unwrap();
        let b = std::env::var("FIDAN_TEST_REGISTRY_B").unwrap();
        command(&["dal", "login", "--token", TOKEN_A, "--registry", &a]).unwrap();
        if case == "explicit-env" {
            command(&["dal", "whoami", "--registry", &b]).unwrap();
            return;
        }
        // Prevent a vacuous negative proof caused by never reading saved credentials.
        assert!(
            command(&["dal", "whoami", "--registry", &a]).is_ok(),
            "a stored login must work at its own registry"
        );
        assert!(command(&["dal", "whoami", "--registry", &b]).is_err());
        if case == "environment" {
            assert!(command(&["dal", "whoami"]).is_err());
        }
        command(&["dal", "login", "--token", TOKEN_B, "--registry", &b]).unwrap();
        command(&["dal", "whoami", "--registry", &b]).unwrap();
        command(&["dal", "logout", "--registry", &a]).unwrap();
        assert!(command(&["dal", "whoami", "--registry", &a]).is_err());
        command(&["dal", "whoami", "--registry", &b]).unwrap();
        command(&["dal", "logout", "--registry", &b]).unwrap();
        assert!(command(&["dal", "whoami", "--registry", &b]).is_err());
        return;
    }
    for case in ["explicit", "environment", "explicit-env"] {
        let a = Registry::new();
        let b = Registry::new();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "dal::credential_tests::registry_scoped_credentials",
                "--nocapture",
            ])
            .env("FIDAN_TEST_CREDENTIAL_CASE", case)
            .env("FIDAN_TEST_REGISTRY_A", &a.address)
            .env("FIDAN_TEST_REGISTRY_B", &b.address)
            .env_remove("FIDAN_DAL_API_TOKEN")
            .env_remove("FIDAN_DAL_REGISTRY");
        if case == "environment" {
            child.env("FIDAN_DAL_REGISTRY", &b.address);
        }
        if case == "explicit-env" {
            child.env("FIDAN_DAL_API_TOKEN", ENV_TOKEN);
        }
        let result = child.output().unwrap();
        assert!(
            result.status.success(),
            "credential fixture {case} failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let received_a = a.headers.lock().unwrap();
        let received_b = b.headers.lock().unwrap();
        assert!(received_a.iter().any(|header| header.contains(TOKEN_A)));
        assert!(
            !received_b.iter().any(|header| header.contains(TOKEN_A)),
            "registry B must never receive A's stored bearer"
        );
        if case == "explicit-env" {
            assert!(received_b.iter().any(|header| header.contains(ENV_TOKEN)));
        }
    }
}

#[test]
fn canonical_registry_identity_rejects_unsafe_urls_and_normalizes_origins() {
    for url in [
        "http://example.com",
        "https://user:pass@example.com",
        "https://example.com?token=x",
        "https://example.com#fragment",
        "file:///tmp/registry",
    ] {
        assert!(
            super::config::resolve_registry(Some(url)).is_err(),
            "unsafe registry must fail before credential lookup"
        );
    }
    for (input, expected) in [
        (
            "HTTPS://API.DAL.FIDAN.DEV:443/",
            "https://api.dal.fidan.dev",
        ),
        (
            "https://example.com/prefix///",
            "https://example.com/prefix",
        ),
        ("http://127.0.0.1:80/prefix/", "http://127.0.0.1/prefix"),
    ] {
        assert_eq!(
            super::config::resolve_registry(Some(input)).unwrap(),
            expected
        );
    }
}

#[test]
fn authenticated_requests_never_follow_registry_redirects() {
    let target = Registry::new();
    for location in [
        format!("{}/outside", target.address),
        "/outside-prefix".to_owned(),
    ] {
        let source = Registry::response(Some(location));
        let client = super::api::DalClient::new(
            format!("{}/prefix", source.address),
            Some(TOKEN_A.to_owned()),
        )
        .unwrap();
        assert!(client.whoami().is_err());
        assert_eq!(
            source.headers.lock().unwrap().len(),
            1,
            "same-origin redirect must not replay bearer"
        );
    }
    assert!(target.headers.lock().unwrap().is_empty());
}
