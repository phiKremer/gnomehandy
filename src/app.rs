use std::{error::Error, sync::Arc, time::Duration};

use tokio::{
    sync::{Notify, mpsc},
    time::sleep,
};
use zbus::{Connection, connection};

use crate::{bluetooth, call_events, cli, ringtone, service, sms};

pub(crate) async fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    if arguments.next().as_deref() == Some("--tel") {
        let uri = arguments.next().unwrap_or_default();
        if uri.is_empty() {
            return Err("--tel benötigt eine tel:-URI".into());
        }
        return cli::dial_uri(&uri).await;
    }

    let reconnect = Arc::new(Notify::new());
    let profile_released = Arc::new(Notify::new());
    let bluetooth_status = Arc::new(bluetooth::BluetoothStatus::default());
    let ringtone_player = Arc::new(ringtone::RingtonePlayer::default());
    let call_controller = Arc::new(bluetooth::CallController::default());
    let conversation_audio = Arc::new(bluetooth::ConversationAudio::default());
    let (call_event_sender, call_events) = mpsc::channel(256);

    let system_connection = connection::Builder::system()?
        .serve_at(
            bluetooth::PROFILE_OBJECT_PATH,
            bluetooth::HfpHandsFreeProfile::new(
                Arc::clone(&reconnect),
                Arc::clone(&profile_released),
                Arc::clone(&bluetooth_status),
                call_event_sender,
                Arc::clone(&call_controller),
            ),
        )?
        .build()
        .await?;

    let session_connection = connection::Builder::session()?
        .name("de.pkremer.GnomeHandy")?
        .serve_at(
            "/de/pkremer/GnomeHandy",
            service::HandyService::new(
                system_connection.clone(),
                Arc::clone(&bluetooth_status),
                Arc::clone(&ringtone_player),
                Arc::clone(&call_controller),
            ),
        )?
        .build()
        .await?;

    call_events::spawn_handler(
        call_events,
        session_connection.clone(),
        Arc::clone(&conversation_audio),
        Arc::clone(&call_controller),
    );

    let notification_connection = host_notification_connection(&session_connection).await?;
    sms::spawn_listener(
        system_connection.clone(),
        session_connection.clone(),
        notification_connection,
    );

    supervise_bluetooth(
        system_connection.clone(),
        reconnect,
        profile_released,
        bluetooth_status,
    );

    eprintln!("GNOME Handy Rust-Dienst ist bereit");
    tokio::select! {
        _ = wait_for_bus_shutdown(&session_connection, "Session-Bus") => {},
        _ = wait_for_bus_shutdown(&system_connection, "System-Bus") => {},
    }
    eprintln!("D-Bus-Verbindung beendet; GNOME Handy wird neu gestartet");
    drop(session_connection);
    drop(system_connection);
    Ok(())
}

async fn host_notification_connection(session: &Connection) -> zbus::Result<Connection> {
    match std::env::var("HOST_SESSION_BUS_ADDRESS") {
        Ok(address) if !address.is_empty() => {
            match connection::Builder::address(address.as_str()) {
                Ok(builder) => match builder.build().await {
                    Ok(connection) => Ok(connection),
                    Err(error) => {
                        eprintln!(
                            "Host-Session-Bus nicht erreichbar; Benachrichtigungen gehen an den aktuellen Bus: {error}"
                        );
                        Ok(session.clone())
                    }
                },
                Err(error) => {
                    eprintln!(
                        "Ungültige Host-Session-Bus-Adresse; verwende den aktuellen Bus: {error}"
                    );
                    Ok(session.clone())
                }
            }
        }
        _ => Ok(session.clone()),
    }
}

fn supervise_bluetooth(
    connection: Connection,
    reconnect: Arc<Notify>,
    profile_released: Arc<Notify>,
    status: Arc<bluetooth::BluetoothStatus>,
) {
    crate::runtime::spawn_critical("Bluetooth-Registrierung", async move {
        let phone_connection = connection.clone();
        let phone_reconnect = Arc::clone(&reconnect);
        crate::runtime::spawn_critical("Handysuche", async move {
            find_phone_and_start_connection_worker(&phone_connection, &phone_reconnect, &status)
                .await;
            std::future::pending::<()>().await;
        });
        loop {
            if register_profile_until_ready(&connection).await {
                reconnect.notify_one();
            }
            // A BlueZ crash may remove the profile without calling Release.
            tokio::select! {
                _ = profile_released.notified() => sleep(Duration::from_secs(2)).await,
                _ = sleep(Duration::from_secs(30)) => {},
            }
        }
    });
}

async fn find_phone_and_start_connection_worker(
    connection: &Connection,
    reconnect: &Arc<Notify>,
    status: &Arc<bluetooth::BluetoothStatus>,
) -> bool {
    let mut retry_delay = Duration::from_secs(2);
    loop {
        match bluetooth::connect_single_paired_phone(
            connection,
            Arc::clone(reconnect),
            Arc::clone(status),
        )
        .await
        {
            Ok(true) => return true,
            Ok(false) => {
                retry_delay = Duration::from_secs(30);
            }
            Err(error) => {
                eprintln!(
                    "HFP-Geräteliste konnte nicht gelesen werden: {error}; erneuter Versuch in {retry_delay:?}"
                );
                retry_delay = (retry_delay * 2).min(Duration::from_secs(60));
            }
        }
        sleep(retry_delay).await;
    }
}

async fn register_profile_until_ready(connection: &Connection) -> bool {
    let mut retry_delay = Duration::from_secs(2);
    loop {
        match bluetooth::register_handsfree_profile(connection).await {
            Ok(()) => return true,
            Err(error) if is_already_registered(&error) => return false,
            Err(error) => {
                eprintln!(
                    "BlueZ-HFP-HF konnte nicht registriert werden: {error}; neuer Versuch in {retry_delay:?}"
                );
                sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(Duration::from_secs(60));
            }
        }
    }
}

fn is_already_registered(error: &zbus::Error) -> bool {
    match error {
        zbus::Error::MethodError(name, message, _) => {
            name.as_str().ends_with(".AlreadyExists")
                || message.as_deref().is_some_and(|message| {
                    message.to_ascii_lowercase().contains("already registered")
                })
        }
        _ => false,
    }
}

async fn wait_for_bus_shutdown(connection: &Connection, name: &str) {
    loop {
        sleep(Duration::from_secs(2)).await;
        let result = connection
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "GetId",
                &(),
            )
            .await;
        if let Err(error) = result {
            eprintln!("{name} nicht mehr erreichbar: {error}");
            return;
        }
    }
}
