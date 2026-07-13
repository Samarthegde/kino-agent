//! Windows service integration.
//!
//! The SCM launches our binary with the command line stored in the service's
//! ImagePath, so `Args::parse()` still sees `--relay-url` / `--agent-id` exactly
//! as it would on a console. What's different is that the process has no console:
//! it must report status back to the SCM and stop when told, which is what this
//! module wires up. Logs go to `--log-file`, since stdout goes nowhere.

use std::ffi::OsString;
use std::sync::mpsc;
use std::time::Duration;

use clap::Parser;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_dispatcher;

use crate::{init_logging, run_agent, Args};

pub const SERVICE_NAME: &str = "kino-agent";

windows_service::define_windows_service!(ffi_service_main, service_main);

/// Try to attach to the SCM. Returns true if we ran as a service (and have now
/// stopped); false if we aren't running under the SCM, in which case the caller
/// should carry on in normal foreground mode.
pub fn try_run_as_service() -> bool {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).is_ok()
}

fn service_main(_scm_args: Vec<OsString>) {
    // Nowhere useful to report an error to at this point - the SCM has already
    // been told we exited, and a service has no console.
    let _ = run_service();
}

fn run_service() -> Result<(), windows_service::Error> {
    // The control handler runs on an SCM thread; it hands stop requests to the
    // async side through this channel.
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

    let status_handle = service_control_handler::register(SERVICE_NAME, move |control| match control
    {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = shutdown_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        // Required: the SCM uses this to poll that we're alive.
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;

    let running = ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    };
    status_handle.set_service_status(running.clone())?;

    let args = Args::parse();
    init_logging(args.log_file.as_deref());

    let runtime = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
    runtime.block_on(async move {
        // Bridge the blocking SCM channel into an async shutdown future.
        let shutdown = async move {
            let _ = tokio::task::spawn_blocking(move || shutdown_rx.recv()).await;
        };
        run_agent(args, shutdown).await;
    });

    status_handle.set_service_status(ServiceStatus {
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        ..running
    })?;

    Ok(())
}
