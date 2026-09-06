//! Paid-request guard for the opt-in real E2E harness.
//!
//! This module is compiled only with `real-e2e-harness`.  It is still inert
//! unless the harness runtime switch is present.  It deliberately does not
//! store provider configuration, URLs, request bodies, or responses: its
//! durable audit has only the kind and ordinal of a generation POST.

use std::{
    fs::OpenOptions,
    io::Write,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};

use serde_json::json;

const AUDIT_FILE: &str = "real-e2e-request-budget.audit.jsonl";
const SOURCE_OPERATION: &str = "novel.analysis";
const ADAPTATION_OPERATION: &str = "novel.adaptation_analysis";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaidKind {
    Source,
    Adaptation,
    Image,
}

/// Immutable per-run ceilings selected by the feature-only harness before
/// any production worker is allowed to reach a provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BudgetProfile {
    limits: [u8; 3],
}

impl BudgetProfile {
    pub(crate) const fn fresh() -> Self {
        Self { limits: [1, 1, 1] }
    }

    /// A stage-retry clone retains an already-paid source analysis.  It may
    /// only issue one replacement adaptation request and one image request.
    pub(crate) const fn resume_stage_retry() -> Self {
        Self { limits: [0, 1, 1] }
    }

    /// An image-only continuation may reuse only a verified ready adaptation.
    pub(crate) const fn image_only_continuation() -> Self {
        Self { limits: [0, 0, 1] }
    }
}

impl PaidKind {
    const fn index(self) -> usize {
        match self {
            Self::Source => 0,
            Self::Adaptation => 1,
            Self::Image => 2,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Adaptation => "adaptation",
            Self::Image => "image",
        }
    }
}

#[derive(Clone)]
struct BudgetGate {
    audit_path: Arc<std::path::PathBuf>,
    used: Arc<Mutex<[u8; 3]>>,
    limits: [u8; 3],
}

impl BudgetGate {
    fn create(root: &Path, profile: BudgetProfile) -> Result<Self, String> {
        let audit_path = root.join(AUDIT_FILE);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&audit_path)
            .map_err(|_| "E2E_REQUEST_BUDGET_AUDIT_CREATE_FAILED".to_string())?;
        file.sync_all()
            .map_err(|_| "E2E_REQUEST_BUDGET_AUDIT_CREATE_FAILED".to_string())?;
        Ok(Self {
            audit_path: Arc::new(audit_path),
            used: Arc::new(Mutex::new([0; 3])),
            limits: profile.limits,
        })
    }

    fn event(&self, event: &str, kind: PaidKind) -> Result<(), String> {
        let line = serde_json::to_string(&json!({
            "event": event,
            "kind": kind.name(),
            "ordinal": 1,
        }))
        .map_err(|_| "E2E_REQUEST_BUDGET_AUDIT_WRITE_FAILED".to_string())?;
        let mut file = OpenOptions::new()
            .append(true)
            .open(self.audit_path.as_ref())
            .map_err(|_| "E2E_REQUEST_BUDGET_AUDIT_WRITE_FAILED".to_string())?;
        writeln!(file, "{line}")
            .and_then(|_| file.sync_data())
            .map_err(|_| "E2E_REQUEST_BUDGET_AUDIT_WRITE_FAILED".to_string())
    }

    fn reserve(&self, kind: PaidKind) -> Result<(), String> {
        let mut used = self
            .used
            .lock()
            .map_err(|_| "E2E_REQUEST_BUDGET_UNAVAILABLE".to_string())?;
        let index = kind.index();
        if used[index] >= self.limits[index] {
            return Err(format!(
                "E2E_{}_REQUEST_BUDGET_EXCEEDED",
                kind.name().to_ascii_uppercase()
            ));
        }

        // The audit record is made durable before the request may leave this
        // process.  A local pre-submit failure still consumes this one-shot
        // acceptance budget rather than inviting a hidden retry.
        used[index] = 1;
        // Fail closed even when persisting the reservation itself fails.  The
        // in-process one-shot budget remains consumed; this process must not
        // turn an audit outage into a second provider POST.
        self.event("generation_post_reserved", kind)
    }

    fn mark_http_received(&self, kind: PaidKind) -> Result<(), String> {
        self.event("generation_http_received", kind)
    }
}

static GATE: OnceLock<Mutex<Option<BudgetGate>>> = OnceLock::new();

fn gate_slot() -> &'static Mutex<Option<BudgetGate>> {
    GATE.get_or_init(|| Mutex::new(None))
}

fn harness_requested() -> bool {
    crate::real_e2e_harness::requested()
}

/// Must be called by the claimed isolated harness before any production job
/// is created.  The marker prevents a later process from reusing this audit.
pub(crate) fn initialize(root: &Path, profile: BudgetProfile) -> Result<(), String> {
    if !harness_requested() {
        return Err("E2E_RUNTIME_SWITCH_REQUIRED".into());
    }
    let gate = BudgetGate::create(root, profile)?;
    let mut slot = gate_slot()
        .lock()
        .map_err(|_| "E2E_REQUEST_BUDGET_UNAVAILABLE".to_string())?;
    if slot.is_some() {
        return Err("E2E_REQUEST_BUDGET_ALREADY_INITIALIZED".into());
    }
    *slot = Some(gate);
    Ok(())
}

fn reserve(kind: PaidKind) -> Result<(), String> {
    if !harness_requested() {
        return Ok(());
    }
    let gate = gate_slot()
        .lock()
        .map_err(|_| "E2E_REQUEST_BUDGET_UNAVAILABLE".to_string())?
        .clone()
        .ok_or("E2E_REQUEST_BUDGET_NOT_INITIALIZED")?;
    gate.reserve(kind)
}

pub(crate) fn reserve_llm_post(operation: &str) -> Result<(), String> {
    if !harness_requested() {
        return Ok(());
    }
    reserve(llm_kind(operation)?)
}

pub(crate) fn reserve_image_post() -> Result<(), String> {
    reserve(PaidKind::Image)
}

fn mark_http_received(kind: PaidKind) -> Result<(), String> {
    if !harness_requested() {
        return Ok(());
    }
    let gate = gate_slot()
        .lock()
        .map_err(|_| "E2E_REQUEST_BUDGET_UNAVAILABLE".to_string())?
        .clone()
        .ok_or("E2E_REQUEST_BUDGET_NOT_INITIALIZED")?;
    gate.mark_http_received(kind)
}

pub(crate) fn mark_llm_http_received(operation: &str) -> Result<(), String> {
    if !harness_requested() {
        return Ok(());
    }
    mark_http_received(llm_kind(operation)?)
}

pub(crate) fn mark_image_http_received() -> Result<(), String> {
    mark_http_received(PaidKind::Image)
}

/// Generation clients in a paid harness run neither follow redirects nor use
/// reqwest's low-level automatic retry policy.  Asset downloads use a
/// separately constructed unprivileged client in `gateway.rs`.
pub(crate) fn generation_client() -> Result<reqwest::Client, String> {
    if !harness_requested() {
        return Ok(reqwest::Client::new());
    }
    restricted_generation_client()
}

fn restricted_generation_client() -> Result<reqwest::Client, String> {
    generation_client_builder()
        .build()
        .map_err(|_| "E2E_GENERATION_CLIENT_BUILD_FAILED".into())
}

fn generation_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
}

pub(crate) fn stream_fallback_allowed() -> bool {
    !harness_requested()
}

fn llm_kind(operation: &str) -> Result<PaidKind, String> {
    match operation {
        SOURCE_OPERATION => Ok(PaidKind::Source),
        ADAPTATION_OPERATION => Ok(PaidKind::Adaptation),
        _ => Err("E2E_UNBUDGETED_LLM_OPERATION".into()),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{Arc, Barrier},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        generation_client_builder, llm_kind, BudgetGate, BudgetProfile, PaidKind, AUDIT_FILE,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::{timeout, Duration},
    };

    fn temp_root() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "image-client-harness-transport-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        root
    }

    fn restricted_generation_test_client() -> reqwest::Client {
        generation_client_builder()
            .no_proxy()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap()
    }

    #[test]
    fn concurrent_reservations_allow_exactly_one_of_each_kind_and_audit_it() {
        let root = temp_root();
        let gate = BudgetGate::create(&root, BudgetProfile::fresh()).unwrap();
        let start = Arc::new(Barrier::new(3));
        let workers = (0..2)
            .map(|_| {
                let gate = gate.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    gate.reserve(PaidKind::Source)
                })
            })
            .collect::<Vec<_>>();
        start.wait();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert!(gate.reserve(PaidKind::Adaptation).is_ok());
        assert!(gate.reserve(PaidKind::Image).is_ok());
        let lines = fs::read_to_string(root.join(AUDIT_FILE)).unwrap();
        assert_eq!(lines.lines().count(), 3);
        assert!(lines.contains("\"source\""));
        assert!(lines.contains("\"adaptation\""));
        assert!(lines.contains("\"image\""));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stage_retry_profile_rejects_source_before_any_audit_or_post() {
        let root = temp_root();
        let gate = BudgetGate::create(&root, BudgetProfile::resume_stage_retry()).unwrap();
        assert_eq!(
            gate.reserve(PaidKind::Source).unwrap_err(),
            "E2E_SOURCE_REQUEST_BUDGET_EXCEEDED"
        );
        assert!(fs::read_to_string(root.join(AUDIT_FILE))
            .unwrap()
            .trim()
            .is_empty());
        assert!(gate.reserve(PaidKind::Adaptation).is_ok());
        assert!(gate.reserve(PaidKind::Image).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn image_only_profile_rejects_all_text_before_audit_and_allows_one_image() {
        let root = temp_root();
        let gate = BudgetGate::create(&root, BudgetProfile::image_only_continuation()).unwrap();
        assert_eq!(
            gate.reserve(PaidKind::Source).unwrap_err(),
            "E2E_SOURCE_REQUEST_BUDGET_EXCEEDED"
        );
        assert_eq!(
            gate.reserve(PaidKind::Adaptation).unwrap_err(),
            "E2E_ADAPTATION_REQUEST_BUDGET_EXCEEDED"
        );
        assert!(fs::read_to_string(root.join(AUDIT_FILE))
            .unwrap()
            .trim()
            .is_empty());
        assert!(gate.reserve(PaidKind::Image).is_ok());
        assert_eq!(
            gate.reserve(PaidKind::Image).unwrap_err(),
            "E2E_IMAGE_REQUEST_BUDGET_EXCEEDED"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_the_two_formal_text_production_operations_have_a_budget_kind() {
        assert_eq!(llm_kind("novel.analysis").unwrap(), PaidKind::Source);
        assert_eq!(
            llm_kind("novel.adaptation_analysis").unwrap(),
            PaidKind::Adaptation
        );
        assert_eq!(
            llm_kind("agent.unrelated").unwrap_err(),
            "E2E_UNBUDGETED_LLM_OPERATION"
        );
    }

    #[tokio::test]
    async fn restricted_generation_client_does_not_follow_307_or_308() {
        for status in ["307 Temporary Redirect", "308 Permanent Redirect"] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let mut task = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request).await.unwrap();
                let response = format!(
                    "HTTP/1.1 {status}\r\nLocation: http://{address}/redirect-target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.flush().await.unwrap();
                timeout(Duration::from_millis(150), listener.accept())
                    .await
                    .is_ok()
            });
            let response = timeout(
                Duration::from_secs(1),
                restricted_generation_test_client()
                    .post(format!("http://{address}/generation"))
                    .body("local")
                    .send(),
            )
            .await
            .expect("redirect request must not hang")
            .unwrap();
            assert_eq!(
                response.status().as_u16(),
                status[..3].parse::<u16>().unwrap()
            );
            let followed = match timeout(Duration::from_secs(1), &mut task).await {
                Ok(Ok(followed)) => followed,
                Ok(Err(error)) => panic!("redirect loopback task failed: {error}"),
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    panic!("redirect loopback task did not terminate");
                }
            };
            assert!(
                !followed,
                "restricted client must not issue a redirect follow-up POST"
            );
        }
    }

    #[tokio::test]
    async fn connection_failure_does_not_release_a_reserved_generation_budget() {
        let root = temp_root();
        let gate = BudgetGate::create(&root, BudgetProfile::fresh()).unwrap();
        gate.reserve(PaidKind::Image).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        assert!(timeout(
            Duration::from_secs(1),
            restricted_generation_test_client()
                .post(format!("http://{address}/generation"))
                .body("local")
                .send(),
        )
        .await
        .expect("connection failure must return promptly")
        .is_err());
        assert!(gate.reserve(PaidKind::Image).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
