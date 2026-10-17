use std::{
    error::Error,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use apalis::{
    layers::WorkerBuilderExt,
    layers::retry::RetryPolicy,
    prelude::{Data, Error as ApalisError, Monitor, Storage, WorkerBuilder, WorkerFactoryFn},
};
use apalis_sql::{
    Config,
    postgres::{PgPool, PostgresStorage},
};
use serde::{Deserialize, Serialize};
use sqlx::{Row, postgres::PgPoolOptions};
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SyntheticJob {
    run_id: String,
    job_key: String,
    behavior: JobBehavior,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum JobBehavior {
    FailOnce,
    BlockAfterFirstEffect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueueState {
    status: String,
    attempts: i32,
    locked: bool,
}

fn timestamp_id() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos()
        .to_string()
}

fn namespace(run_id: &str) -> String {
    format!("litecr.queue-prototype.{run_id}")
}

async fn ensure_prototype_schema(pool: &PgPool) -> Result<(), Box<dyn Error>> {
    sqlx::query("CREATE SCHEMA IF NOT EXISTS queue_prototype")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS queue_prototype.runs (
            run_id TEXT PRIMARY KEY,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS queue_prototype.effects (
            run_id TEXT NOT NULL,
            job_key TEXT NOT NULL,
            effect_count INTEGER NOT NULL DEFAULT 1 CHECK (effect_count = 1),
            handler_runs INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (run_id, job_key)
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn handle(job: SyntheticJob, pool: Data<PgPool>) -> Result<(), ApalisError> {
    let row = sqlx::query(
        "INSERT INTO queue_prototype.effects (run_id, job_key, handler_runs)
         VALUES ($1, $2, 1)
         ON CONFLICT (run_id, job_key)
         DO UPDATE SET handler_runs = queue_prototype.effects.handler_runs + 1
         RETURNING handler_runs, effect_count",
    )
    .bind(&job.run_id)
    .bind(&job.job_key)
    .fetch_one(&*pool)
    .await
    .map_err(|error| ApalisError::Failed(Arc::new(Box::new(error))))?;
    let handler_runs: i32 = row.get("handler_runs");
    let effect_count: i32 = row.get("effect_count");
    info!(job_key = %job.job_key, handler_runs, effect_count, "synthetic job entered");

    match job.behavior {
        JobBehavior::FailOnce if handler_runs == 1 => Err(ApalisError::Failed(Arc::new(Box::new(
            std::io::Error::other("synthetic first-attempt failure"),
        )))),
        JobBehavior::BlockAfterFirstEffect if handler_runs == 1 => loop {
            sleep(Duration::from_secs(60)).await;
        },
        _ => Ok(()),
    }
}

async fn worker(database_url: &str, run_id: &str, worker_name: &str) -> Result<(), Box<dyn Error>> {
    let pool = PgPool::connect(database_url).await?;
    let config = Config::new(&namespace(run_id))
        .set_buffer_size(1)
        .set_poll_interval(Duration::from_millis(100))
        .set_keep_alive(Duration::from_millis(300))
        .set_reenqueue_orphaned_after(Duration::from_millis(300));
    let storage = PostgresStorage::<SyntheticJob>::new_with_config(pool.clone(), config);
    Monitor::new()
        .register(
            WorkerBuilder::new(worker_name)
                .data(pool)
                .retry(RetryPolicy::retries(3))
                .backend(storage)
                .build_fn(handle),
        )
        .run()
        .await?;
    Ok(())
}

async fn enqueue(
    pool: &PgPool,
    run_id: &str,
    job_key: &str,
    behavior: JobBehavior,
) -> Result<String, Box<dyn Error>> {
    let config = Config::new(&namespace(run_id)).set_buffer_size(1);
    let mut storage = PostgresStorage::<SyntheticJob>::new_with_config(pool.clone(), config);
    let parts = storage
        .push(SyntheticJob {
            run_id: run_id.to_owned(),
            job_key: job_key.to_owned(),
            behavior,
        })
        .await?;
    Ok(parts.task_id.to_string())
}

async fn queue_state(pool: &PgPool, job_id: &str) -> Result<Option<QueueState>, sqlx::Error> {
    let row = sqlx::query("SELECT status, attempts, lock_by FROM apalis.jobs WHERE id = $1")
        .bind(job_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|row| QueueState {
        status: row.get("status"),
        attempts: row.get("attempts"),
        locked: row.get::<Option<String>, _>("lock_by").is_some(),
    }))
}

async fn effect_state(
    pool: &PgPool,
    run_id: &str,
    job_key: &str,
) -> Result<Option<(i32, i32)>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT effect_count, handler_runs FROM queue_prototype.effects WHERE run_id = $1 AND job_key = $2",
    )
    .bind(run_id)
    .bind(job_key)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| (row.get("effect_count"), row.get("handler_runs"))))
}

async fn wait_for<F, Fut>(description: &str, mut check: F) -> Result<(), Box<dyn Error>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool, Box<dyn Error>>>,
{
    timeout(Duration::from_secs(20), async {
        loop {
            if check().await? {
                return Ok::<(), Box<dyn Error>>(());
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| format!("timed out waiting for {description}"))??;
    Ok(())
}

fn spawn_worker(
    executable: &std::path::Path,
    run_id: &str,
    worker_name: &str,
    database_url: &str,
) -> Result<Child, Box<dyn Error>> {
    Ok(Command::new(executable)
        .arg("worker")
        .arg(run_id)
        .arg(worker_name)
        .env("LITECR_QUEUE_DATABASE_URL", database_url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?)
}

async fn prove_failure_retry(
    pool: &PgPool,
    executable: &std::path::Path,
    database_url: &str,
    run_id: &str,
) -> Result<(), Box<dyn Error>> {
    let job_id = enqueue(pool, run_id, "retry-once", JobBehavior::FailOnce).await?;
    let mut process = spawn_worker(executable, run_id, "retry-worker", database_url)?;
    let wait_result = wait_for("automatic retry to complete", || async {
        let state = queue_state(pool, &job_id).await?;
        let effects = effect_state(pool, run_id, "retry-once").await?;
        Ok(
            state.is_some_and(|state| state.status == "Done" && state.attempts >= 1)
                && effects.is_some_and(|(effect_count, handler_runs)| {
                    effect_count == 1 && handler_runs == 2
                }),
        )
    })
    .await;
    process.kill()?;
    let _ = process.wait();
    wait_result?;
    let state = queue_state(pool, &job_id)
        .await?
        .ok_or("retried job disappeared")?;
    let effects = effect_state(pool, run_id, "retry-once")
        .await?
        .ok_or("retry receipt missing")?;
    if state.status != "Done" || effects != (1, 2) {
        return Err(format!("retry assertion failed: state={state:?}, effect={effects:?}").into());
    }
    println!(
        "retry proof: job={job_id}, status={}, attempts={}, handler_runs={}, idempotent_effects={}",
        state.status, state.attempts, effects.1, effects.0
    );
    Ok(())
}

async fn prove_kill_recovery(
    pool: &PgPool,
    executable: &std::path::Path,
    database_url: &str,
    run_id: &str,
) -> Result<(), Box<dyn Error>> {
    let job_id = enqueue(
        pool,
        run_id,
        "kill-recover",
        JobBehavior::BlockAfterFirstEffect,
    )
    .await?;
    let mut crashed = spawn_worker(executable, run_id, "worker-before-kill", database_url)?;
    let wait_result = wait_for("worker to claim and start synthetic job", || async {
        let state = queue_state(pool, &job_id).await?;
        let effects = effect_state(pool, run_id, "kill-recover").await?;
        Ok(
            state.is_some_and(|state| state.status == "Running" && state.locked)
                && effects.is_some_and(|(_, handler_runs)| handler_runs == 1),
        )
    })
    .await;
    if let Err(error) = wait_result {
        let _ = crashed.kill();
        let _ = crashed.wait();
        return Err(error);
    }

    crashed.kill()?;
    let exit = crashed.wait()?;
    if exit.success() {
        return Err("forced worker termination unexpectedly exited successfully".into());
    }
    let abandoned = queue_state(pool, &job_id)
        .await?
        .ok_or("abandoned job disappeared")?;
    if abandoned.status != "Running" || !abandoned.locked {
        return Err(format!("kill did not leave a locked running job: {abandoned:?}").into());
    }

    let mut recovery = spawn_worker(executable, run_id, "worker-after-kill", database_url)?;
    let wait_result = wait_for("orphan lease recovery and completion", || async {
        let state = queue_state(pool, &job_id).await?;
        let effects = effect_state(pool, run_id, "kill-recover").await?;
        Ok(state.is_some_and(|state| state.status == "Done")
            && effects
                .is_some_and(|(effect_count, handler_runs)| effect_count == 1 && handler_runs >= 2))
    })
    .await;
    let _ = recovery.kill();
    let _ = recovery.wait();
    wait_result?;
    let state = queue_state(pool, &job_id)
        .await?
        .ok_or("recovered job disappeared")?;
    let effects = effect_state(pool, run_id, "kill-recover")
        .await?
        .ok_or("recovery receipt missing")?;
    if state.status != "Done" || effects.0 != 1 || effects.1 < 2 {
        return Err(
            format!("recovery assertion failed: state={state:?}, effect={effects:?}").into(),
        );
    }
    println!(
        "kill-recovery proof: job={job_id}, killed_status=Running, final_status={}, attempts={}, handler_runs={}, idempotent_effects={}",
        state.status, state.attempts, effects.1, effects.0
    );
    Ok(())
}

async fn run_prototype(database_url: &str) -> Result<(), Box<dyn Error>> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;
    PostgresStorage::<()>::setup(&pool).await?;
    ensure_prototype_schema(&pool).await?;
    let run_id = timestamp_id();
    sqlx::query("INSERT INTO queue_prototype.runs (run_id) VALUES ($1)")
        .bind(&run_id)
        .execute(&pool)
        .await?;
    let executable = std::env::current_exe()?;
    println!(
        "queue prototype: apalis 0.7.4 + apalis-sql 0.7.4 + SQLx 0.8.6, PostgreSQL-backed, namespace={}",
        namespace(&run_id)
    );

    let result = async {
        prove_failure_retry(&pool, &executable, database_url, &run_id).await?;
        prove_kill_recovery(&pool, &executable, database_url, &run_id).await?;
        Ok::<(), Box<dyn Error>>(())
    }
    .await;

    if result.is_ok() {
        sqlx::query("DELETE FROM apalis.jobs WHERE job_type = $1")
            .bind(namespace(&run_id))
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM apalis.workers WHERE worker_type = $1")
            .bind(namespace(&run_id))
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM queue_prototype.runs WHERE run_id = $1")
            .bind(&run_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM queue_prototype.effects WHERE run_id = $1")
            .bind(&run_id)
            .execute(&pool)
            .await?;
    } else {
        warn!(
            run_id,
            "queue prototype failed; keeping this run's synthetic rows for diagnosis"
        );
    }
    result
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("warn,litecr_queue_prototype=info")
        .init();
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("worker") if args.len() == 4 => {
            let database_url = std::env::var("LITECR_QUEUE_DATABASE_URL")?;
            worker(&database_url, &args[2], &args[3]).await
        }
        Some("run") => {
            let database_url = std::env::var("LITECR_QUEUE_DATABASE_URL")?;
            run_prototype(&database_url).await
        }
        _ => Err("usage: litecr-queue-prototype run (set LITECR_QUEUE_DATABASE_URL)".into()),
    }
}
