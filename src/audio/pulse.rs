use std::{
    thread::sleep,
    time::{Duration, Instant},
};

use libpulse_binding as pulse;
use pulse::{
    context::{Context, State as ContextState},
    mainloop::standard::{IterateResult, Mainloop},
    operation::{Operation, State as OperationState},
};
use zbus::fdo;

const PULSE_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const PULSE_POLL_INTERVAL: Duration = Duration::from_millis(5);

pub(super) fn wait_for_context(mainloop: &mut Mainloop, context: &Context) -> fdo::Result<()> {
    let deadline = Instant::now() + PULSE_OPERATION_TIMEOUT;
    while context.get_state() != ContextState::Ready {
        if matches!(
            context.get_state(),
            ContextState::Failed | ContextState::Terminated
        ) {
            return Err(fdo::Error::Failed(
                "PulseAudio/PipeWire ist nicht erreichbar (läuft pipewire-pulse?)".into(),
            ));
        }
        if Instant::now() >= deadline {
            return Err(fdo::Error::Failed(
                "Zeitüberschreitung beim Verbinden mit PulseAudio/PipeWire".into(),
            ));
        }
        if matches!(
            mainloop.iterate(false),
            IterateResult::Err(_) | IterateResult::Quit(_)
        ) {
            return Err(fdo::Error::Failed(
                "PulseAudio-Mainloop ist fehlgeschlagen".into(),
            ));
        }
        sleep(PULSE_POLL_INTERVAL);
    }
    Ok(())
}

pub(super) fn wait_for_operation<Callback: ?Sized>(
    mainloop: &mut Mainloop,
    operation: &Operation<Callback>,
    error_message: &str,
) -> fdo::Result<()> {
    let deadline = Instant::now() + PULSE_OPERATION_TIMEOUT;
    while operation.get_state() == OperationState::Running {
        if Instant::now() >= deadline {
            return Err(fdo::Error::Failed(format!(
                "{error_message}: Zeitüberschreitung"
            )));
        }
        if matches!(
            mainloop.iterate(false),
            IterateResult::Err(_) | IterateResult::Quit(_)
        ) {
            return Err(fdo::Error::Failed(error_message.into()));
        }
        sleep(PULSE_POLL_INTERVAL);
    }
    if operation.get_state() != OperationState::Done {
        return Err(fdo::Error::Failed(error_message.into()));
    }
    Ok(())
}
