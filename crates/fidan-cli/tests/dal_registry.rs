//! Actual CLI processes against a loopback-only registry fixture.
use flate2::{Compression, write::GzEncoder};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static NEXT_SANDBOX: AtomicU64 = AtomicU64::new(0);

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "fidan-registry-audit-{}-{nonce}-{}",
            std::process::id(),
            NEXT_SANDBOX.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::write(
            path.join("dal.toml"),
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        Self(path)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Registry {
    base: String,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Registry {
    fn new(routes: BTreeMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let worker_stop = stop.clone();
        let worker_requests = requests.clone();
        let worker = thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                if worker_stop.load(Ordering::SeqCst) {
                    break;
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                worker_requests.lock().unwrap().push(path.clone());
                loop {
                    line.clear();
                    if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                let (status, body) = routes
                    .get(&path)
                    .map_or((404, b"not found".as_slice()), |body| {
                        (200, body.as_slice())
                    });
                let header = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                if stream.write_all(header.as_bytes()).is_ok() {
                    let _ = stream.write_all(body);
                }
            }
        });
        Self {
            base: format!("http://{address}"),
            stop,
            requests,
            worker: Some(worker),
        }
    }
}
impl Drop for Registry {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.base.trim_start_matches("http://"));
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn package(name: &str, version: &str, compiler: Option<&str>, dependencies: &str) -> Vec<u8> {
    package_with_source(
        name,
        version,
        compiler,
        dependencies,
        "action value returns integer { return 42 }\n",
        None,
    )
}
fn package_with_source(
    name: &str,
    version: &str,
    compiler: Option<&str>,
    dependencies: &str,
    source: &str,
    lock: Option<&str>,
) -> Vec<u8> {
    let requirement = compiler
        .map(|value| format!("fidan = \"{value}\"\n"))
        .unwrap_or_default();
    let manifest = format!(
        "[package]\nname = \"{name}\"\nversion = \"{version}\"\n{requirement}{dependencies}"
    );
    let source = source.as_bytes();
    let mut tar = tar::Builder::new(Vec::new());
    let mut files = vec![("dal.toml", manifest.as_bytes()), ("src/init.fdn", source)];
    if let Some(lock) = lock {
        files.push(("dal.lock", lock.as_bytes()));
    }
    for (file, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, format!("{name}-{version}/{file}"), bytes)
            .unwrap();
    }
    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(&tar.into_inner().unwrap()).unwrap();
    gzip.finish().unwrap()
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn routes(archive: Vec<u8>, mut entry: serde_json::Value) -> BTreeMap<String, Vec<u8>> {
    if entry.get("cksum").is_none() {
        entry["cksum"] = digest(&archive).into();
    }
    BTreeMap::from([
        (
            "/index/demo".to_string(),
            serde_json::to_vec(&entry).unwrap(),
        ),
        (
            "/packages/demo/versions/1.0.0/download".to_string(),
            archive,
        ),
    ])
}
fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fidan"))
        .args(args)
        .current_dir(root)
        .env("FIDAN_HOME", root.join("isolated-home"))
        .env("FIDAN_DAL_API_TOKEN", "test-only-token")
        .output()
        .unwrap()
}
fn add(root: &Path, registry: &Registry, force: bool) -> Output {
    let mut args = vec!["dal", "add", "demo", "--registry", &registry.base];
    if force {
        args.push("--force");
    }
    cli(root, &args)
}
fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn real_cli_verifies_installs_imports_and_rejects_modified_cache() {
    let sandbox = Sandbox::new();
    let requirement = format!("={}", env!("CARGO_PKG_VERSION"));
    let registry = Registry::new(routes(
        package("demo", "1.0.0", Some(&requirement), ""),
        serde_json::json!({"name":"demo", "vers":"1.0.0", "yanked":false, "fidan":requirement}),
    ));
    assert_success(&add(&sandbox.0, &registry, false));
    let installed = fidan_driver::dal::package_install_dir(
        &fidan_driver::dal::local_package_store(&sandbox.0),
        "demo",
        "1.0.0",
    );
    assert!(installed.join("init.fdn").is_file());
    fs::write(
        sandbox.0.join("main.fdn"),
        "use demo\nprint(demo.value())\n",
    )
    .unwrap();
    let run = cli(&sandbox.0, &["run", "main.fdn"]);
    assert_success(&run);
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "42");
    fs::write(installed.join("init.fdn"), "tampered").unwrap();
    let rejected = add(&sandbox.0, &registry, false);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("differs from verified archive"));
    assert_eq!(
        fs::read_to_string(installed.join("init.fdn")).unwrap(),
        "tampered"
    );
    assert_success(&add(&sandbox.0, &registry, true));
    assert_success(&cli(&sandbox.0, &["run", "main.fdn"]));
}

#[test]
fn real_cli_rejects_checksum_tampering_and_wrong_archive_identity_before_install() {
    for (archive_name, checksum) in [("demo", "0".repeat(64)), ("other", String::new())] {
        let sandbox = Sandbox::new();
        let bytes = package(archive_name, "1.0.0", None, "");
        let checksum = if checksum.is_empty() {
            digest(&bytes)
        } else {
            checksum
        };
        let registry = Registry::new(routes(
            bytes,
            serde_json::json!({"name":"demo", "vers":"1.0.0", "yanked":false,"cksum":checksum}),
        ));
        let result = add(&sandbox.0, &registry, false);
        assert!(!result.status.success());
        assert!(!fidan_driver::dal::local_package_store(&sandbox.0).exists());
        assert!(!sandbox.0.join("dal.lock").exists());
    }
}

#[test]
fn real_cli_rejects_malformed_index_without_downloading_and_incompatible_archive() {
    for entry in [
        serde_json::json!({"name":"demo","vers":"1.0.0","yanked":false,"cksum":"bad"}),
        serde_json::json!({"name":"other","vers":"1.0.0","yanked":false}),
    ] {
        let sandbox = Sandbox::new();
        let registry = Registry::new(routes(package("demo", "1.0.0", None, ""), entry));
        assert!(!add(&sandbox.0, &registry, false).status.success());
        assert_eq!(
            registry.requests.lock().unwrap().as_slice(),
            ["/index/demo"]
        );
    }
    let sandbox = Sandbox::new();
    let registry = Registry::new(routes(
        package("demo", "1.0.0", Some(">=99"), ""),
        serde_json::json!({"name":"demo","vers":"1.0.0","yanked":false}),
    ));
    let result = add(&sandbox.0, &registry, false);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("requires Fidan"));
    assert!(!sandbox.0.join("dal.lock").exists());
}

fn graph_registry(packages: &[(&str, &str, &str, bool)]) -> Registry {
    let mut routes = BTreeMap::new();
    let mut indexes: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for &(name, version, deps, yanked) in packages {
        let bytes = package(name, version, None, deps);
        indexes.entry(name.to_string()).or_default().push(
            serde_json::json!({"name":name,"vers":version,"yanked":yanked,"cksum":digest(&bytes)}),
        );
        routes.insert(
            format!("/packages/{name}/versions/{version}/download"),
            bytes,
        );
    }
    for (name, entries) in indexes {
        routes.insert(
            format!("/index/{name}"),
            entries
                .iter()
                .map(|e| serde_json::to_string(e).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
                .into_bytes(),
        );
    }
    Registry::new(routes)
}
#[test]
fn resolver_backtracks_versions_and_discards_rejected_edges() {
    let sandbox = Sandbox::new();
    // Greedy chooses a=2 then b=2; c requires b=1. A valid graph needs a=1.
    let registry = graph_registry(&[
        (
            "demo",
            "1.0.0",
            "[dependencies]\na = \">=1, <3\"\nc = \"1.0.0\"\n",
            false,
        ),
        (
            "a",
            "2.0.0",
            "[dependencies]\nb = \"2.0.0\"\nrejected = \"1.0.0\"\n",
            false,
        ),
        ("a", "1.0.0", "[dependencies]\nb = \"1.0.0\"\n", false),
        ("b", "2.0.0", "", false),
        ("b", "1.0.0", "", false),
        ("c", "1.0.0", "[dependencies]\nb = \"1.0.0\"\n", false),
    ]);
    assert_success(&add(&sandbox.0, &registry, false));
    let lock = fidan_driver::dal::read_lock(&sandbox.0).unwrap();
    assert_eq!(
        lock.packages
            .iter()
            .find(|p| p.name == "a")
            .unwrap()
            .version,
        "1.0.0"
    );
    assert_eq!(
        lock.packages
            .iter()
            .find(|p| p.name == "b")
            .unwrap()
            .version,
        "1.0.0"
    );
    assert!(!lock.packages.iter().any(|p| p.name == "rejected"));
    assert!(
        lock.packages
            .iter()
            .all(|p| p.checksum.as_ref().is_some_and(|v| v.len() == 64))
    );
}
#[test]
fn resolver_unifies_features_and_rejects_cycles_and_true_conflicts() {
    let sandbox = Sandbox::new();
    let registry = graph_registry(&[
        (
            "demo",
            "1.0.0",
            "[dependencies]\na = {version=\"1.0.0\",features=[\"extra\"]}\nb = \"1.0.0\"\n",
            false,
        ),
        ("b", "1.0.0", "[dependencies]\na = \"1.0.0\"\n", false),
        (
            "a",
            "1.0.0",
            "[optional-dependencies]\nc = \"1.0.0\"\n[features]\nextra=[\"dep:c\"]\n",
            false,
        ),
        ("c", "1.0.0", "", false),
    ]);
    assert_success(&add(&sandbox.0, &registry, false));
    let lock = fidan_driver::dal::read_lock(&sandbox.0).unwrap();
    assert_eq!(lock.packages.len(), 4);
    assert_eq!(
        lock.packages
            .iter()
            .find(|p| p.name == "a")
            .unwrap()
            .features,
        vec!["extra"]
    );
    for packages in [
        vec![
            ("demo", "1.0.0", "[dependencies]\na=\"1.0.0\"\n", false),
            ("a", "1.0.0", "[dependencies]\ndemo=\"1.0.0\"\n", false),
        ],
        vec![
            (
                "demo",
                "1.0.0",
                "[dependencies]\na=\"1.0.0\"\nb=\"1.0.0\"\n",
                false,
            ),
            ("a", "1.0.0", "[dependencies]\nb=\"2.0.0\"\n", false),
            ("b", "1.0.0", "", false),
            ("b", "2.0.0", "", false),
        ],
    ] {
        let fresh = Sandbox::new();
        let registry = graph_registry(&packages);
        assert!(!add(&fresh.0, &registry, false).status.success());
        assert!(!fresh.0.join("dal.lock").exists());
    }
}
#[test]
fn locked_yanked_artifact_is_reproducible_but_new_or_changed_artifact_is_rejected() {
    let sandbox = Sandbox::new();
    let registry = graph_registry(&[("demo", "1.0.0", "", false)]);
    assert_success(&add(&sandbox.0, &registry, false));
    let original = fs::read(sandbox.0.join("dal.lock")).unwrap();
    drop(registry);
    let registry = graph_registry(&[("demo", "1.0.0", "", true), ("demo", "2.0.0", "", false)]);
    assert_success(&add(&sandbox.0, &registry, false));
    assert_eq!(original, fs::read(sandbox.0.join("dal.lock")).unwrap());
    let fresh = Sandbox::new();
    assert_success(&add(&fresh.0, &registry, false));
    assert_eq!(
        fidan_driver::dal::read_lock(&fresh.0).unwrap().packages[0].version,
        "2.0.0"
    );
    let only_yanked = graph_registry(&[("demo", "1.0.0", "", true)]);
    assert!(!add(&Sandbox::new().0, &only_yanked, false).status.success());
    let mutated = Registry::new(routes(
        package("demo", "1.0.0", None, ""),
        serde_json::json!({"name":"demo","vers":"1.0.0","yanked":false,"cksum":"a".repeat(64)}),
    ));
    assert!(!add(&sandbox.0, &mutated, false).status.success());
    assert_eq!(original, fs::read(sandbox.0.join("dal.lock")).unwrap());
}
#[test]
fn run_forwards_arguments_with_and_without_separator() {
    let sandbox = Sandbox::new();
    fs::write(
        sandbox.0.join("main.fdn"),
        "use std.env\nvar values = env.args()\nfor i in 0..len(values) { print(values[i]) }\n",
    )
    .unwrap();
    for args in [
        vec!["run", "main.fdn", "--", "--name", "Dal", "--verbose"],
        vec!["run", "main.fdn", "--name", "Dal", "--verbose"],
        vec![
            "run",
            "--trace",
            "none",
            "main.fdn",
            "--name",
            "Dal",
            "--verbose",
        ],
    ] {
        let output = cli(&sandbox.0, &args);
        assert_success(&output);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
            "--name\nDal\n--verbose\n"
        );
    }
}

#[test]
fn imports_reject_incompatible_installed_manifest_and_corrupt_lock() {
    let sandbox = Sandbox::new();
    let registry = graph_registry(&[("demo", "1.0.0", "", false)]);
    assert_success(&add(&sandbox.0, &registry, false));
    fs::write(
        sandbox.0.join("main.fdn"),
        "use demo\nprint(demo.value())\n",
    )
    .unwrap();
    let installed = sandbox.0.join(".dal/packages/demo/1.0.0/dal.toml");
    fs::write(
        &installed,
        "[package]\nname=\"demo\"\nversion=\"1.0.0\"\nfidan=\">=999\"\n",
    )
    .unwrap();
    let output = cli(&sandbox.0, &["run", "main.fdn"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires Fidan"));
    fs::write(sandbox.0.join("dal.lock"), "invalid lock contents").unwrap();
    assert!(!cli(&sandbox.0, &["run", "main.fdn"]).status.success());
}

fn nullable_backend(backend: &str, home: Option<PathBuf>) {
    let sandbox = Sandbox::new();
    fs::write(
        sandbox.0.join("main.fdn"),
        [
            include_str!("../../../test/examples/dal_nullable_value_regression.fdn"),
            include_str!("../../../test/examples/dal_dynamic_conditions_regression.fdn"),
        ]
        .join("\n"),
    )
    .unwrap();
    for threshold in ["0", "1"] {
        let output = cli(
            &sandbox.0,
            &["run", "--jit-threshold", threshold, "main.fdn"],
        );
        assert_success(&output);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "nullable values preserved\ndynamic conditions preserved"
        );
    }
    let artifacts = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/dal-backend-regressions")
        .join(sandbox.0.file_name().unwrap());
    fs::create_dir_all(&artifacts).unwrap();
    let binary = artifacts.join("nullable");
    let mut command = Command::new(env!("CARGO_BIN_EXE_fidan"));
    command
        .current_dir(&sandbox.0)
        .args(["build", "main.fdn", "--backend", backend, "--output"])
        .arg(&binary);
    if backend == "llvm" {
        command.args(["--lto", "full"]);
    }
    command.env(
        "FIDAN_HOME",
        home.unwrap_or_else(|| sandbox.0.join("isolated-home")),
    );
    assert_success(&command.output().unwrap());
    let binary = if cfg!(windows) {
        binary.with_extension("exe")
    } else {
        binary
    };
    let output = Command::new(binary)
        .current_dir(&sandbox.0)
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "nullable values preserved\ndynamic conditions preserved"
    );
}
#[test]
fn nullable_values_preserve_interpreter_jit_and_cranelift_tags() {
    nullable_backend("cranelift", None);
}
#[test]
#[ignore = "requires an already installed compatible LLVM helper; no toolchain provisioning in tests"]
fn nullable_values_preserve_llvm_tags() {
    nullable_backend("llvm", Some(fidan_driver::resolve_fidan_home().unwrap()));
}

#[test]
fn transitive_imports_use_consumer_graph_instead_of_publisher_lock() {
    let sandbox = Sandbox::new();
    let archived_lock =
        "schema_version=1\n[[packages]]\nname=\"leaf\"\nmodule=\"leaf\"\nversion=\"1.0.0\"\n";
    let mut routes = BTreeMap::new();
    let mut indexes: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for (name, version, bytes) in [
        (
            "demo",
            "1.0.0",
            package_with_source(
                "demo",
                "1.0.0",
                None,
                "[dependencies]\nleaf=\"^1\"\n",
                "use leaf\naction value returns integer {return leaf.value()}\n",
                Some(archived_lock),
            ),
        ),
        (
            "leaf",
            "1.0.0",
            package_with_source(
                "leaf",
                "1.0.0",
                None,
                "",
                "action value returns integer {return 10}\n",
                None,
            ),
        ),
        (
            "leaf",
            "1.1.0",
            package_with_source(
                "leaf",
                "1.1.0",
                None,
                "",
                "action value returns integer {return 11}\n",
                None,
            ),
        ),
    ] {
        indexes.entry(name.into()).or_default().push(
            serde_json::json!({"name":name,"vers":version,"yanked":false,"cksum":digest(&bytes)}),
        );
        routes.insert(
            format!("/packages/{name}/versions/{version}/download"),
            bytes,
        );
    }
    for (name, entries) in indexes {
        routes.insert(
            format!("/index/{name}"),
            entries
                .iter()
                .map(|e| serde_json::to_string(e).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
                .into_bytes(),
        );
    }
    let registry = Registry::new(routes);
    assert_success(&add(&sandbox.0, &registry, false));
    assert_eq!(
        fidan_driver::dal::read_lock(&sandbox.0)
            .unwrap()
            .packages
            .iter()
            .find(|p| p.name == "leaf")
            .unwrap()
            .version,
        "1.1.0"
    );
    fs::write(
        sandbox.0.join("main.fdn"),
        "use demo\nassert_eq(demo.value(),11)\n",
    )
    .unwrap();
    assert_success(&cli(&sandbox.0, &["run", "main.fdn"]));
    let output = sandbox.0.join("transitive");
    assert_success(&cli(
        &sandbox.0,
        &[
            "build",
            "main.fdn",
            "--backend",
            "cranelift",
            "--output",
            output.to_str().unwrap(),
        ],
    ));
    let executable = if cfg!(windows) {
        output.with_extension("exe")
    } else {
        output
    };
    assert_success(&Command::new(executable).output().unwrap());
}

#[test]
fn resolver_backtracks_when_a_candidate_depends_on_a_missing_package() {
    let sandbox = Sandbox::new();
    let registry = graph_registry(&[
        ("demo", "2.0.0", "[dependencies]\nmissing=\"^1\"\n", false),
        ("demo", "1.0.0", "", false),
    ]);
    assert_success(&add(&sandbox.0, &registry, false));
    assert_eq!(
        fidan_driver::dal::read_lock(&sandbox.0).unwrap().packages[0].version,
        "1.0.0"
    );
}

fn module_backend(backend: &str, home: Option<PathBuf>) {
    let sandbox = Sandbox::new();
    fs::write(sandbox.0.join("left.fdn"), "use std.string.{reverse}\naction reversed returns string { return reverse(\"ab\") }\nvar bias = 10\naction helper returns integer {return bias}\naction value returns integer {return helper()+1}\n").unwrap();
    fs::write(sandbox.0.join("right.fdn"), "use std.collections.{reverse}\naction reversed returns list oftype integer { return reverse([1,2]) }\nvar bias = 20\naction helper returns integer {return bias}\naction value returns integer {return helper()+2}\n").unwrap();
    fs::write(
        sandbox.0.join("bundle.fdn"),
        "export use left as tools\nexport use std.math as calc\n",
    )
    .unwrap();
    fs::write(sandbox.0.join("main.fdn"), "use left\nuse right\nuse bundle\naction via with (certain namespace oftype flexible) returns integer {return namespace.tools.value()}\nassert_eq(via(bundle),11)\nassert_eq(bundle.calc.sqrt(9.0),3.0)\nassert_eq(left.reversed(),\"ba\")\nassert_eq(right.reversed(),[2,1])\nvar bias = 100\nassert_eq(bias,100)\nvar first oftype action = left.value\nvar second oftype flexible = right.value\nassert_eq(first(),11)\nassert_eq(second(),22)\nassert_eq(left.value(),11)\nassert_eq(right.value(),22)\n").unwrap();
    for threshold in ["0", "1"] {
        assert_success(&cli(
            &sandbox.0,
            &["run", "--jit-threshold", threshold, "main.fdn"],
        ));
    }
    let output = sandbox.0.join("module-functions");
    let mut command = Command::new(env!("CARGO_BIN_EXE_fidan"));
    command
        .current_dir(&sandbox.0)
        .args(["build", "main.fdn", "--backend", backend, "--output"])
        .arg(&output)
        .env(
            "FIDAN_HOME",
            home.unwrap_or_else(|| sandbox.0.join("isolated-home")),
        );
    if backend == "llvm" {
        command.args(["--lto", "full"]);
    }
    assert_success(&command.output().unwrap());
    assert_success(
        &Command::new(if cfg!(windows) {
            output.with_extension("exe")
        } else {
            output
        })
        .output()
        .unwrap(),
    );
}

#[test]
fn imported_actions_keep_private_state_helpers_reexports_and_first_class_identity() {
    module_backend("cranelift", None);
}
#[test]
#[ignore = "requires an already installed compatible LLVM helper; no toolchain provisioning in tests"]
fn imported_modules_preserve_llvm_identity() {
    module_backend("llvm", Some(fidan_driver::resolve_fidan_home().unwrap()));
}

#[test]
fn simultaneous_cli_adds_preserve_both_dependency_roots_and_lock_entries() {
    let registry = graph_registry(&[("alpha", "1.0.0", "", false), ("beta", "1.0.0", "", false)]);
    for _ in 0..5 {
        let sandbox = Sandbox::new();
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                cli(
                    &sandbox.0,
                    &["dal", "add", "alpha", "--registry", &registry.base],
                )
            });
            let second = scope.spawn(|| {
                cli(
                    &sandbox.0,
                    &["dal", "add", "beta", "--registry", &registry.base],
                )
            });
            assert_success(&first.join().unwrap());
            assert_success(&second.join().unwrap());
        });
        let manifest = fidan_driver::dal::read_manifest(&sandbox.0).unwrap();
        assert!(manifest.dependencies.contains_key("alpha"));
        assert!(manifest.dependencies.contains_key("beta"));
        let lock = fidan_driver::dal::read_lock(&sandbox.0).unwrap();
        assert_eq!(
            lock.packages
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
    }
}
#[test]
fn global_remove_rejects_corrupt_lock_without_discarding_roots_or_installation() {
    let sandbox = Sandbox::new();
    let home = sandbox.0.join("isolated-home");
    let dal = home.join("dal");
    fs::create_dir_all(dal.join("packages/demo/1.0.0")).unwrap();
    let roots = "[dependencies]\ndemo=\"^1\"\n";
    fs::write(dal.join("global.toml"), roots).unwrap();
    fs::write(dal.join("global.lock"), "corrupt lock").unwrap();
    assert!(
        !cli(
            &sandbox.0,
            &["dal", "remove", "demo", "--global", "--confirm"]
        )
        .status
        .success()
    );
    assert_eq!(fs::read_to_string(dal.join("global.toml")).unwrap(), roots);
    assert!(dal.join("packages/demo/1.0.0").is_dir());
}

#[test]
fn oversized_registry_json_is_bounded_and_does_not_write_project_state() {
    let sandbox = Sandbox::new();
    let registry = Registry::new(BTreeMap::from([(
        "/packages/demo".into(),
        vec![b' '; 8 * 1024 * 1024 + 1],
    )]));
    let output = cli(
        &sandbox.0,
        &["dal", "info", "demo", "--registry", &registry.base],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("8 MiB limit"));
    assert!(!sandbox.0.join("dal.lock").exists());
}
