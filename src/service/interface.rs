use std::path::PathBuf;
use std::sync::Arc;

use zbus::{Connection, interface, object_server::SignalEmitter};

use crate::{
    audio::{self, AudioDevice, Direction},
    bluetooth::{self, BluetoothStatus, CallController},
    ringtone::RingtonePlayer,
    settings,
};

pub(crate) struct HandyService {
    bluez: Connection,
    bluetooth_status: Arc<BluetoothStatus>,
    ringtone_player: Arc<RingtonePlayer>,
    call_controller: Arc<CallController>,
}

impl HandyService {
    pub(crate) fn new(
        bluez: Connection,
        bluetooth_status: Arc<BluetoothStatus>,
        ringtone_player: Arc<RingtonePlayer>,
        call_controller: Arc<CallController>,
    ) -> Self {
        Self {
            bluez,
            bluetooth_status,
            ringtone_player,
            call_controller,
        }
    }
}

#[interface(name = "de.pkremer.GnomeHandy")]
impl HandyService {
    async fn connect_phone(&self, address: &str) -> zbus::fdo::Result<()> {
        bluetooth::connect_phone(&self.bluez, address).await
    }

    fn get_bluetooth_status(&self) -> zbus::fdo::Result<(bool, String)> {
        self.bluetooth_status.connection()
    }

    async fn list_audio_outputs(&self) -> zbus::fdo::Result<Vec<AudioDevice>> {
        run_blocking("Audioausgänge konnten nicht aufgelistet werden", || {
            audio::list_devices(Direction::Output)
        })
        .await
    }

    async fn list_audio_inputs(&self) -> zbus::fdo::Result<Vec<AudioDevice>> {
        run_blocking("Audioeingänge konnten nicht aufgelistet werden", || {
            audio::list_devices(Direction::Input)
        })
        .await
    }

    fn get_audio_output_selection(&self) -> zbus::fdo::Result<String> {
        settings::get_audio_selection("audio-output", true)
    }

    async fn set_audio_output_selection(&self, name: &str) -> zbus::fdo::Result<()> {
        let name = name.to_string();
        run_blocking(
            "Klingelton-Auswahl konnte nicht gespeichert werden",
            move || settings::save_audio_selection("audio-output", &name, true, Direction::Output),
        )
        .await
    }

    fn get_conversation_output_selection(&self) -> zbus::fdo::Result<String> {
        settings::get_audio_selection("conversation-output", false)
    }

    async fn set_conversation_output_selection(&self, name: &str) -> zbus::fdo::Result<()> {
        let name = name.to_string();
        run_blocking(
            "Gesprächsausgabe konnte nicht gespeichert werden",
            move || {
                settings::save_audio_selection(
                    "conversation-output",
                    &name,
                    false,
                    Direction::Output,
                )
            },
        )
        .await
    }

    fn get_microphone_selection(&self) -> zbus::fdo::Result<String> {
        settings::get_audio_selection("microphone-input", false)
    }

    async fn set_microphone_selection(&self, name: &str) -> zbus::fdo::Result<()> {
        let name = name.to_string();
        run_blocking(
            "Mikrofonauswahl konnte nicht gespeichert werden",
            move || {
                settings::save_audio_selection("microphone-input", &name, false, Direction::Input)
            },
        )
        .await
    }

    async fn start_ringtone(&self, path: &str) -> zbus::fdo::Result<()> {
        let selection = settings::get_audio_selection("audio-output", true)?;
        let ringtone_player = Arc::clone(&self.ringtone_player);
        let path = PathBuf::from(path);
        tokio::task::spawn_blocking(move || {
            let routes = audio::output_routes(&selection)?;
            ringtone_player.start(path, routes);
            Ok(())
        })
        .await
        .map_err(|error| {
            zbus::fdo::Error::Failed(format!("Klingeltonstart ist fehlgeschlagen: {error}"))
        })?
    }

    async fn stop_ringtone(&self) -> zbus::fdo::Result<()> {
        let ringtone_player = Arc::clone(&self.ringtone_player);
        tokio::task::spawn_blocking(move || ringtone_player.stop())
            .await
            .map_err(|error| {
                zbus::fdo::Error::Failed(format!("Klingeltonstopp ist fehlgeschlagen: {error}"))
            })
    }

    fn answer_call(&self) -> zbus::fdo::Result<()> {
        self.call_controller.answer_call()
    }

    fn hang_up_call(&self) -> zbus::fdo::Result<()> {
        self.call_controller.hang_up()
    }

    fn reject_call(&self) -> zbus::fdo::Result<()> {
        self.call_controller.hang_up()
    }

    fn send_dtmf(&self, tone: &str) -> zbus::fdo::Result<()> {
        self.call_controller.send_dtmf(tone)
    }

    fn dial_number(&self, number: &str) -> zbus::fdo::Result<String> {
        self.call_controller.dial_number(number)
    }

    #[zbus(signal)]
    async fn incoming_call(
        emitter: &SignalEmitter<'_>,
        call_id: &str,
        caller_name: &str,
        phone_number: &str,
        details: Vec<(String, String)>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn outgoing_call(
        emitter: &SignalEmitter<'_>,
        call_id: &str,
        phone_number: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn call_ended(emitter: &SignalEmitter<'_>, call_id: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn call_active(emitter: &SignalEmitter<'_>, call_id: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn audio_state(
        emitter: &SignalEmitter<'_>,
        call_id: &str,
        status: &str,
    ) -> zbus::Result<()>;
}

async fn run_blocking<T, F>(operation: &'static str, task: F) -> zbus::fdo::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> zbus::fdo::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|error| zbus::fdo::Error::Failed(format!("{operation}: {error}")))?
}

/// Data published to the GNOME Shell extension when Bluetooth reports a call.
/// Use an empty name or number if the phone does not provide that information.
pub(crate) struct IncomingCall {
    pub call_id: String,
    pub caller_name: String,
    pub phone_number: String,
    pub details: Vec<(String, String)>,
}

/// Emit `IncomingCall`; the extension displays its call layer until `publish_call_ended`.
pub(crate) async fn publish_incoming_call(
    connection: &Connection,
    call: IncomingCall,
) -> zbus::Result<()> {
    let emitter = SignalEmitter::new(connection, "/de/pkremer/GnomeHandy")?;
    HandyService::incoming_call(
        &emitter,
        &call.call_id,
        &call.caller_name,
        &call.phone_number,
        call.details,
    )
    .await
}

/// Emit `CallEnded` for the matching call identifier.
pub(crate) async fn publish_call_ended(connection: &Connection, call_id: &str) -> zbus::Result<()> {
    let emitter = SignalEmitter::new(connection, "/de/pkremer/GnomeHandy")?;
    HandyService::call_ended(&emitter, call_id).await
}

pub(crate) async fn publish_outgoing_call(
    connection: &Connection,
    call_id: &str,
    phone_number: &str,
) -> zbus::Result<()> {
    let emitter = SignalEmitter::new(connection, "/de/pkremer/GnomeHandy")?;
    HandyService::outgoing_call(&emitter, call_id, phone_number).await
}

pub(crate) async fn publish_call_active(
    connection: &Connection,
    call_id: &str,
) -> zbus::Result<()> {
    let emitter = SignalEmitter::new(connection, "/de/pkremer/GnomeHandy")?;
    HandyService::call_active(&emitter, call_id).await
}

pub(crate) async fn publish_audio_state(
    connection: &Connection,
    call_id: &str,
    status: &str,
) -> zbus::Result<()> {
    let emitter = SignalEmitter::new(connection, "/de/pkremer/GnomeHandy")?;
    HandyService::audio_state(&emitter, call_id, status).await
}
