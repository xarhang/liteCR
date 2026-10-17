//! Optional first-party advisory matching worker command.

use std::{env, future::Future, time::Duration};

use litecr_application::worker_runtime::{self, WorkerConfig};
use litecr_domain::SCHEMA_VERSION;
use litecr_infrastructure::{connect, schema_version, worker::PostgresMatchingConsumer};
use sqlx::PgPool;
use tokio::sync::watch;
use tracing::info;
#[cfg(unix)]
use tracing::warn;

const POLL_INTERVAL: Duration = Duration::from_millis(750);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(25);

pub async fn run() -> Result<(), String> {
    run_service(wait_for_shutdown_signal(), || {}).await
}

pub(crate) async fn run_service<S, R>(shutdown: S, on_ready: R) -> Result<(), String>
where
    S: Future<Output = ()> + Send + 'static,
    R: FnOnce() + Send + 'static,
{
    let worker_url = env::var("LITECR_WORKER_DATABASE_URL")
        .map_err(|_| "LITECR_WORKER_DATABASE_URL is required for worker".to_owned())?;
    if worker_url.trim().is_empty() {
        return Err("LITECR_WORKER_DATABASE_URL must not be empty".to_owned());
    }
    if env::var("LITECR_DATABASE_URL")
        .ok()
        .is_some_and(|api_url| api_url == worker_url)
    {
        return Err("worker and API must use separately credentialed database URLs".to_owned());
    }
    let pool = connect(&worker_url, 4).await.map_err(|_| {
        "worker database connection failed; verify worker database configuration".to_owned()
    })?;
    ensure_worker_database_role(&pool).await?;
    ensure_schema(&pool).await?;

    let owner = worker_owner()?;
    let consumer = PostgresMatchingConsumer::new(pool.clone());
    on_ready();
    run_with_shutdown(&consumer, &owner, shutdown).await?;
    pool.close().await;
    Ok(())
}

async fn ensure_worker_database_role(pool: &PgPool) -> Result<(), String> {
    let current_user: String = sqlx::query_scalar("SELECT current_user")
        .fetch_one(pool)
        .await
        .map_err(|_| "worker could not verify its database role".to_owned())?;
    require_worker_database_role(&current_user)
}

fn require_worker_database_role(current_user: &str) -> Result<(), String> {
    if current_user == "litecr_worker" {
        Ok(())
    } else {
        Err("worker database identity must be exactly litecr_worker".to_owned())
    }
}

async fn ensure_schema(pool: &PgPool) -> Result<(), String> {
    let version = schema_version(pool).await.map_err(|_| {
        "worker could not query liteCR schema metadata; verify database permissions and health"
            .to_owned()
    })?;
    match version {
        Some(version) if version.value() == SCHEMA_VERSION => Ok(()),
        Some(version) => Err(format!(
            "worker schema {} must exactly match binary schema {}; run the matching litecr migrate release",
            version.value(),
            SCHEMA_VERSION
        )),
        None => Err("liteCR schema is not installed; run litecr migrate".to_owned()),
    }
}

fn worker_owner() -> Result<String, String> {
    let node = env::var("LITECR_WORKER_ID")
        .unwrap_or_else(|_| env::var("HOSTNAME").unwrap_or_else(|_| "worker".to_owned()));
    worker_owner_from_node(&node, std::process::id())
}

fn worker_owner_from_node(node: &str, process_id: u32) -> Result<String, String> {
    if node.trim().is_empty()
        || node.len() > 100
        || node.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err("LITECR_WORKER_ID must contain 1 to 100 printable bytes".to_owned());
    }
    Ok(format!("{node}-{process_id}"))
}

async fn run_with_shutdown<C, S>(consumer: &C, owner: &str, shutdown: S) -> Result<(), String>
where
    C: litecr_application::worker::AdvisoryMatchingConsumer,
    S: Future<Output = ()> + Send + 'static,
{
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        shutdown.await;
        let _ = shutdown_tx.send(true);
    });

    run_loop(consumer, owner, &mut shutdown_rx).await
}

async fn run_loop<C: litecr_application::worker::AdvisoryMatchingConsumer>(
    consumer: &C,
    owner: &str,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    loop {
        if *shutdown_rx.borrow() {
            info!("worker shutdown received; no more claims will be made");
            return Ok(());
        }
        let batch = tokio::time::timeout(
            DRAIN_TIMEOUT,
            worker_runtime::run_batch(consumer, owner, WorkerConfig::default()),
        )
        .await
        .map_err(|_| {
            "worker drain exceeded 25 seconds; unfinished leases will recover after lease expiry"
                .to_owned()
        })?
        .map_err(str::to_owned)?;
        if batch.claimed > 0 {
            info!(
                claimed = batch.claimed,
                completed = batch.completed,
                obsolete = batch.obsolete,
                retried = batch.retried,
                retry_exhausted = batch.retry_exhausted,
                lease_lost = batch.lease_lost,
                deferred = batch.deferred_until_lease_expiry,
                "worker batch finished"
            );
            continue;
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL_INTERVAL) => {},
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    info!("worker shutdown received while idle");
                    return Ok(());
                }
            }
        }
    }
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            warn!("could not install SIGTERM handler; Ctrl-C remains available");
            let _ = tokio::signal::ctrl_c().await;
            return;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use litecr_application::worker::{AdvisoryMatchingConsumer, ConsumerCompletion, ConsumerError};
    use litecr_domain::worker::ConsumerLease;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::sync::oneshot;

    struct DrainingConsumer {
        shutdown: Arc<Mutex<Option<oneshot::Sender<()>>>>,
        claims: AtomicUsize,
        evaluations: AtomicUsize,
    }

    #[async_trait]
    impl AdvisoryMatchingConsumer for DrainingConsumer {
        async fn claim(
            &self,
            owner: &str,
            limit: usize,
            _lease_seconds: i64,
            _max_attempts: i32,
        ) -> Result<Vec<ConsumerLease>, ConsumerError> {
            assert_eq!(owner, "synthetic-worker");
            assert_eq!(limit, 1);
            self.claims.fetch_add(1, Ordering::SeqCst);
            Ok(vec![ConsumerLease {
                event_id: "synthetic-event".to_owned(),
                registry_id: "synthetic-registry".to_owned(),
                organization_id: "synthetic-org".to_owned(),
                source_record_id: "synthetic-source".to_owned(),
                expected_receive_version: 1,
                attempt: 1,
                lease_owner: "synthetic-worker".to_owned(),
                lease_token: "synthetic-token".to_owned(),
            }])
        }

        async fn evaluate_and_complete(
            &self,
            _lease: &ConsumerLease,
            _candidate_limit: usize,
        ) -> Result<ConsumerCompletion, ConsumerError> {
            self.evaluations.fetch_add(1, Ordering::SeqCst);
            if let Some(sender) = self.shutdown.lock().unwrap().take() {
                let _ = sender.send(());
            }
            tokio::task::yield_now().await;
            Ok(ConsumerCompletion::EvidencePersisted {
                evidence_run_id: "synthetic-run".to_owned(),
            })
        }

        async fn retry(
            &self,
            _lease: &ConsumerLease,
            _error_code: &str,
            _max_attempts: i32,
            _base_delay_seconds: i64,
        ) -> Result<bool, ConsumerError> {
            Ok(false)
        }
    }

    #[tokio::test]
    async fn injected_signal_during_evaluation_drains_lease_then_stops_before_next_claim() {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let consumer = DrainingConsumer {
            shutdown: Arc::new(Mutex::new(Some(shutdown_tx))),
            claims: AtomicUsize::new(0),
            evaluations: AtomicUsize::new(0),
        };
        tokio::time::timeout(
            Duration::from_secs(2),
            run_with_shutdown(&consumer, "synthetic-worker", async move {
                let _ = shutdown_rx.await;
            }),
        )
        .await
        .expect("injected shutdown is observed after current lease drains")
        .unwrap();
        assert_eq!(consumer.claims.load(Ordering::SeqCst), 1);
        assert_eq!(consumer.evaluations.load(Ordering::SeqCst), 1);
    }

    struct IdleConsumer {
        claims: AtomicUsize,
        first_claim: Mutex<Option<oneshot::Sender<()>>>,
    }

    #[async_trait]
    impl AdvisoryMatchingConsumer for IdleConsumer {
        async fn claim(
            &self,
            owner: &str,
            limit: usize,
            _lease_seconds: i64,
            _max_attempts: i32,
        ) -> Result<Vec<ConsumerLease>, ConsumerError> {
            assert_eq!(owner, "synthetic-worker");
            assert_eq!(limit, 1);
            self.claims.fetch_add(1, Ordering::SeqCst);
            if let Some(sender) = self.first_claim.lock().unwrap().take() {
                let _ = sender.send(());
            }
            Ok(Vec::new())
        }

        async fn evaluate_and_complete(
            &self,
            _lease: &ConsumerLease,
            _candidate_limit: usize,
        ) -> Result<ConsumerCompletion, ConsumerError> {
            unreachable!("idle consumer has no leases to evaluate")
        }

        async fn retry(
            &self,
            _lease: &ConsumerLease,
            _error_code: &str,
            _max_attempts: i32,
            _base_delay_seconds: i64,
        ) -> Result<bool, ConsumerError> {
            unreachable!("idle consumer has no leases to retry")
        }
    }

    #[tokio::test]
    async fn injected_signal_stops_idle_worker_without_waiting_for_poll_interval() {
        let (claimed_tx, claimed_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let consumer = Arc::new(IdleConsumer {
            claims: AtomicUsize::new(0),
            first_claim: Mutex::new(Some(claimed_tx)),
        });
        let worker_consumer = Arc::clone(&consumer);
        let worker = tokio::spawn(async move {
            run_with_shutdown(worker_consumer.as_ref(), "synthetic-worker", async move {
                let _ = shutdown_rx.await;
            })
            .await
        });

        claimed_rx.await.expect("worker made its first idle claim");
        shutdown_tx
            .send(())
            .expect("shutdown future is still active");
        tokio::time::timeout(Duration::from_millis(250), worker)
            .await
            .expect("injected shutdown interrupts idle polling")
            .expect("worker task joins")
            .expect("worker stops cleanly");
        assert_eq!(consumer.claims.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn owner_rejects_control_bytes_and_has_process_unique_suffix() {
        assert_eq!(
            worker_owner_from_node("synthetic-worker", 7).unwrap(),
            "synthetic-worker-7"
        );
        assert!(worker_owner_from_node("bad\nname", 7).is_err());
        assert!(worker_owner_from_node(" ", 7).is_err());
    }

    #[test]
    fn database_identity_requires_the_dedicated_worker_role() {
        assert!(require_worker_database_role("litecr_worker").is_ok());
        assert!(require_worker_database_role("litecr_api").is_err());
        assert!(require_worker_database_role("postgres").is_err());
    }
}
