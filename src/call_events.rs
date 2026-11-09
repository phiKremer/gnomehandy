use std::sync::Arc;

use tokio::sync::mpsc::Receiver;
use zbus::Connection;

use crate::{
    bluetooth::{CallController, CallEvent, ConversationAudio},
    service::{self, IncomingCall},
    settings,
};

pub(crate) fn spawn_handler(
    mut events: Receiver<CallEvent>,
    connection: Connection,
    audio: Arc<ConversationAudio>,
    call_controller: Arc<CallController>,
) {
    crate::runtime::spawn_critical("Anrufereignisse", async move {
        while let Some(event) = events.recv().await {
            let result = match event {
                CallEvent::Incoming {
                    call_id,
                    phone_number,
                } => {
                    service::publish_incoming_call(
                        &connection,
                        IncomingCall {
                            call_id,
                            caller_name: String::new(),
                            phone_number,
                            details: Vec::new(),
                        },
                    )
                    .await
                }
                CallEvent::Outgoing {
                    call_id,
                    phone_number,
                } => service::publish_outgoing_call(&connection, &call_id, &phone_number).await,
                CallEvent::Ended { call_id } => {
                    audio.stop_call(&call_id);
                    service::publish_call_ended(&connection, &call_id).await
                }
                CallEvent::Active { call_id } => {
                    handle_active_call(
                        &connection,
                        &call_id,
                        Arc::clone(&audio),
                        Arc::clone(&call_controller),
                    )
                    .await
                }
            };

            if let Err(error) = result {
                eprintln!("HFP-Anrufereignis konnte nicht an GNOME gesendet werden: {error}");
            }
        }
    });
}

async fn handle_active_call(
    connection: &Connection,
    call_id: &str,
    audio: Arc<ConversationAudio>,
    call_controller: Arc<CallController>,
) -> zbus::Result<()> {
    if let Err(error) = call_controller.request_audio_connection() {
        eprintln!("HFP-Audioverbindung konnte nicht angefordert werden: {error}");
    }

    let active_result = service::publish_call_active(connection, call_id).await;
    let audio_call_id = call_id.to_string();
    let audio_result =
        tokio::task::spawn_blocking(move || start_conversation_audio(&audio, &audio_call_id)).await;

    let audio_status = match audio_result {
        Ok(Ok(())) => "Gesprächsaudio verbunden".to_string(),
        Ok(Err(error)) => format!("Kein Gesprächsaudio: {error}"),
        Err(error) => format!("Gesprächsaudio fehlgeschlagen: {error}"),
    };
    let audio_signal_result =
        service::publish_audio_state(connection, call_id, &audio_status).await;

    active_result.and(audio_signal_result)
}

fn start_conversation_audio(audio: &ConversationAudio, call_id: &str) -> Result<(), String> {
    let (device_path, _) = call_id
        .split_once('#')
        .ok_or_else(|| "Ungültige HFP-Anrufkennung".to_string())?;
    let device_address = device_path
        .rsplit("dev_")
        .next()
        .ok_or_else(|| "Bluetooth-Geräteadresse fehlt in der Anrufkennung".to_string())?
        .replace('_', ":");
    let output = settings::get_audio_selection("conversation-output", false)
        .map_err(|error| error.to_string())?;
    let input = settings::get_audio_selection("microphone-input", false)
        .map_err(|error| error.to_string())?;

    audio.start(call_id, &device_address, &output, &input)
}
