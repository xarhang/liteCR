//! Windows Service Control Manager adapter for the existing API and worker shutdown paths.
//!
//! This module does not install services or run migrations. An external, reviewed Windows
//! package must register the binary with `service-api` or `service-worker` and the matching
//! service name.

use std::{ffi::OsString, future::Future, pin::Pin, time::Duration};

use tokio::sync::watch;
use tracing::{error, info};
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle},
    service_dispatcher,
};

const API_SERVICE_NAME: &str = "litecr-api";
const WORKER_SERVICE_NAME: &str = "litecr-worker";
const START_WAIT_HINT: Duration = Duration::from_secs(60);
const STOP_WAIT_HINT: Duration = Duration::from_secs(30);
const STOP_CHECK_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServiceMode {
    Api,
    Worker,
}

impl ServiceMode {
    fn service_name(self) -> &'static str {
        match self {
            Self::Api => API_SERVICE_NAME,
            Self::Worker => WORKER_SERVICE_NAME,
        }
    }
}

pub(crate) fn dispatch(mode: ServiceMode) -> Result<(), String> {
    let entry = match mode {
        ServiceMode::Api => service_dispatcher::start(API_SERVICE_NAME, ffi_api_service_main),
        ServiceMode::Worker => {
            service_dispatcher::start(WORKER_SERVICE_NAME, ffi_worker_service_main)
        }
    };
    entry.map_err(|error| format!("could not connect liteCR service to Windows SCM: {error}"))
}

define_windows_service!(ffi_api_service_main, api_service_main);
define_windows_service!(ffi_worker_service_main, worker_service_main);

fn api_service_main(_arguments: Vec<OsString>) {
    if let Err(error) = run_service(ServiceMode::Api) {
        error!(%error, "liteCR API Windows service stopped with an error");
    }
}

fn worker_service_main(_arguments: Vec<OsString>) {
    if let Err(error) = run_service(ServiceMode::Worker) {
        error!(%error, "liteCR worker Windows service stopped with an error");
    }
}

fn run_service(mode: ServiceMode) -> Result<(), String> {
    let (stop_tx, stop_rx) = watch::channel(false);
    let handler_stop_tx = stop_tx.clone();
    let handler = move |control| match control {
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        ServiceControl::Stop | ServiceControl::Shutdown => {
            handler_stop_tx.send_replace(true);
            ServiceControlHandlerResult::NoError
        }
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(mode.service_name(), handler)
        .map_err(|error| format!("could not register liteCR SCM control handler: {error}"))?;
    if let Err(error) = report_status(
        &status,
        ServiceState::StartPending,
        ServiceControlAccept::empty(),
        1,
        START_WAIT_HINT,
    ) {
        let _ = report_stopped(&status, false);
        return Err(error);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            let _ = report_stopped(&status, false);
            "could not start liteCR service runtime".to_owned()
        })?;
    runtime.block_on(run_service_runtime(mode, status, stop_rx, stop_tx))
}

async fn run_service_runtime(
    mode: ServiceMode,
    status: ServiceStatusHandle,
    mut stop_state: watch::Receiver<bool>,
    stop_tx: watch::Sender<bool>,
) -> Result<(), String> {
    let (ready_tx, mut ready_rx) = tokio::sync::oneshot::channel();
    let task_stop_state = stop_state.clone();
    let mut task = tokio::spawn(async move {
        let ready = move || {
            let _ = ready_tx.send(());
        };
        run_mode(mode, stop_future(task_stop_state), ready).await
    });

    let mut startup_checkpoint = 1_u32;
    loop {
        tokio::select! {
            ready = &mut ready_rx => {
                if ready.is_ok() {
                    break;
                }
                let result = join_result(task.await);
                report_stopped(&status, false)?;
                return match result {
                    Ok(()) => Err("liteCR service exited before reaching readiness".to_owned()),
                    Err(error) => Err(error),
                };
            }
            result = &mut task => {
                let result = join_result(result);
                report_stopped(&status, false)?;
                return match result {
                    Ok(()) => Err("liteCR service exited before reaching readiness".to_owned()),
                    Err(error) => Err(error),
                };
            }
            _ = tokio::time::sleep(STOP_CHECK_INTERVAL) => {
                startup_checkpoint = startup_checkpoint.saturating_add(1);
                if let Err(error) = report_status(&status, ServiceState::StartPending, ServiceControlAccept::empty(), startup_checkpoint, START_WAIT_HINT) {
                    stop_tx.send_replace(true);
                    let _ = join_result(task.await);
                    let _ = report_stopped(&status, false);
                    return Err(error);
                }
            }
        }
    }

    if let Err(error) = report_status(
        &status,
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        0,
        Duration::default(),
    ) {
        stop_tx.send_replace(true);
        let _ = join_result(task.await);
        let _ = report_stopped(&status, false);
        return Err(error);
    }
    info!(
        service = mode.service_name(),
        "liteCR Windows service is running"
    );

    let mut stopping = false;
    let mut checkpoint = 1_u32;
    let result = loop {
        if stopping {
            tokio::select! {
                result = &mut task => break join_result(result),
                _ = tokio::time::sleep(STOP_CHECK_INTERVAL) => {
                    checkpoint = checkpoint.saturating_add(1);
                    let _ = report_status(&status, ServiceState::StopPending, ServiceControlAccept::empty(), checkpoint, STOP_WAIT_HINT);
                }
            }
        } else {
            tokio::select! {
                result = &mut task => break join_result(result),
                changed = stop_state.changed() => {
                    if changed.is_err() || *stop_state.borrow() {
                        stopping = true;
                        let _ = report_status(&status, ServiceState::StopPending, ServiceControlAccept::empty(), 1, STOP_WAIT_HINT);
                    }
                }
            }
        }
    };

    let requested_stop = *stop_state.borrow();
    let success = requested_stop && result.is_ok();
    report_stopped(&status, success)?;
    if !success && result.is_ok() {
        return Err("liteCR service exited without a successful SCM stop".to_owned());
    }
    result
}

fn join_result(result: Result<Result<(), String>, tokio::task::JoinError>) -> Result<(), String> {
    result.map_err(|_| "liteCR service task failed".to_owned())?
}

async fn stop_future(mut state: watch::Receiver<bool>) {
    loop {
        if *state.borrow() {
            return;
        }
        if state.changed().await.is_err() {
            return;
        }
    }
}

type ServiceFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

fn run_mode<R>(
    mode: ServiceMode,
    stop: impl Future<Output = ()> + Send + 'static,
    ready: R,
) -> ServiceFuture
where
    R: FnOnce() + Send + 'static,
{
    match mode {
        ServiceMode::Api => Box::pin(crate::run_api_with_shutdown(stop, ready)),
        ServiceMode::Worker => Box::pin(crate::worker_cli::run_service(stop, ready)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_and_worker_use_distinct_scm_service_names() {
        assert_eq!(ServiceMode::Api.service_name(), "litecr-api");
        assert_eq!(ServiceMode::Worker.service_name(), "litecr-worker");
        assert_ne!(
            ServiceMode::Api.service_name(),
            ServiceMode::Worker.service_name()
        );
    }
}

fn report_stopped(status: &ServiceStatusHandle, success: bool) -> Result<(), String> {
    status
        .set_service_status(status_value(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            0,
            Duration::default(),
            success,
        ))
        .map_err(|error| format!("could not report liteCR SCM stopped status: {error}"))
}

fn report_status(
    status: &ServiceStatusHandle,
    state: ServiceState,
    controls: ServiceControlAccept,
    checkpoint: u32,
    wait_hint: Duration,
) -> Result<(), String> {
    status
        .set_service_status(status_value(state, controls, checkpoint, wait_hint, true))
        .map_err(|error| format!("could not report liteCR SCM status {state:?}: {error}"))
}

fn status_value(
    state: ServiceState,
    controls: ServiceControlAccept,
    checkpoint: u32,
    wait_hint: Duration,
    success: bool,
) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: controls,
        exit_code: ServiceExitCode::Win32(if success { 0 } else { 1 }),
        checkpoint,
        wait_hint,
        process_id: None,
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[test]
    fn status_contract_reports_truthful_start_run_stop_and_failure_states() {
        let startup = status_value(
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
            1,
            START_WAIT_HINT,
            true,
        );
        assert_eq!(startup.current_state, ServiceState::StartPending);
        assert!(startup.controls_accepted.is_empty());
        assert_eq!(startup.checkpoint, 1);
        assert_eq!(startup.wait_hint, START_WAIT_HINT);

        let running = status_value(
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            0,
            Duration::default(),
            true,
        );
        assert_eq!(running.current_state, ServiceState::Running);
        assert_eq!(
            running.controls_accepted,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        );
        assert_eq!(running.checkpoint, 0);

        let stopping = status_value(
            ServiceState::StopPending,
            ServiceControlAccept::empty(),
            2,
            STOP_WAIT_HINT,
            true,
        );
        assert_eq!(stopping.current_state, ServiceState::StopPending);
        assert!(stopping.controls_accepted.is_empty());
        assert_eq!(stopping.checkpoint, 2);
        assert_eq!(stopping.wait_hint, STOP_WAIT_HINT);

        let stopped = status_value(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            0,
            Duration::default(),
            true,
        );
        let failed = status_value(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            0,
            Duration::default(),
            false,
        );
        assert_eq!(stopped.exit_code, ServiceExitCode::Win32(0));
        assert_eq!(failed.exit_code, ServiceExitCode::Win32(1));
    }
}
