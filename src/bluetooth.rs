mod hfp;

use std::{collections::HashMap, sync::Arc, sync::Mutex, time::Duration};

use tokio::{sync::Notify, time::sleep};
use zbus::{
    Connection,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

pub(crate) use hfp::{CallController, CallEvent, ConversationAudio, HfpHandsFreeProfile};

pub(crate) const PROFILE_OBJECT_PATH: &str = "/de/pkremer/GnomeHandy/BluezHfp";

const HFP_HANDSFREE_UUID: &str = "0000111e-0000-1000-8000-00805f9b34fb";
const HFP_AUDIO_GATEWAY_UUID: &str = "0000111f-0000-1000-8000-00805f9b34fb";

#[derive(Default)]
pub(crate) struct BluetoothStatus {
    state: Mutex<BluetoothStatusState>,
}

#[derive(Default)]
struct BluetoothStatusState {
    device_names: HashMap<String, String>,
    connected_device_path: Option<String>,
}

impl BluetoothStatus {
    fn add_device(&self, path: String, name: String) {
        if let Ok(mut state) = self.state.lock() {
            state.device_names.insert(path, name);
        }
    }

    pub(crate) fn set_connected_device(&self, path: String) {
        if let Ok(mut state) = self.state.lock() {
            state.connected_device_path = Some(path);
        }
    }

    pub(crate) fn clear_connected_device(&self, path: &str) {
        if let Ok(mut state) = self.state.lock() {
            if state.connected_device_path.as_deref() == Some(path) {
                state.connected_device_path = None;
            }
        }
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.connected_device_path = None;
        }
    }

    pub(crate) fn connection(&self) -> zbus::fdo::Result<(bool, String)> {
        let state = self
            .state
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("Bluetooth-Status ist nicht verfügbar".into()))?;
        let Some(path) = state.connected_device_path.as_ref() else {
            return Ok((false, String::new()));
        };
        let name = state
            .device_names
            .get(path)
            .cloned()
            .unwrap_or_else(|| path.clone());
        Ok((true, name))
    }
}

pub(crate) async fn register_handsfree_profile(connection: &Connection) -> zbus::Result<()> {
    let mut options = HashMap::<String, OwnedValue>::new();
    options.insert("Role".into(), OwnedValue::try_from(Value::from("client"))?);
    options.insert("RequireAuthentication".into(), OwnedValue::from(true));
    options.insert("AutoConnect".into(), OwnedValue::from(true));

    connection
        .call_method(
            Some("org.bluez"),
            "/org/bluez",
            Some("org.bluez.ProfileManager1"),
            "RegisterProfile",
            &(
                OwnedObjectPath::try_from(PROFILE_OBJECT_PATH)?,
                HFP_HANDSFREE_UUID,
                options,
            ),
        )
        .await?;
    eprintln!("BlueZ HFP Hands-Free (HF) registriert");
    Ok(())
}

pub(crate) async fn connect_phone(connection: &Connection, address: &str) -> zbus::fdo::Result<()> {
    let normalized = address.replace(':', "_").to_ascii_uppercase();
    if normalized.len() != 17
        || !normalized.bytes().enumerate().all(|(index, byte)| {
            if index % 3 == 2 {
                byte == b'_'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Err(zbus::fdo::Error::InvalidArgs(
            "Bluetooth-Adresse muss das Format AA:BB:CC:DD:EE:FF haben".into(),
        ));
    }

    let device_path = format!("/org/bluez/hci0/dev_{normalized}");
    connect_hfp_profile(connection, &device_path)
        .await
        .map_err(|error| {
            zbus::fdo::Error::Failed(format!(
                "HFP-Verbindung zu {address} konnte nicht aufgebaut werden: {error}"
            ))
        })
}

pub(crate) async fn connect_single_paired_phone(
    connection: &Connection,
    reconnect: Arc<Notify>,
    status: Arc<BluetoothStatus>,
) -> zbus::Result<bool> {
    let phones = paired_hfp_phones(connection, &status).await?;
    let Some(phone) = select_phone(&phones) else {
        log_no_phone_selection(&phones);
        return Ok(false);
    };

    spawn_hfp_connection_worker(connection.clone(), phone.path.to_string(), reconnect);
    Ok(true)
}

struct PairedPhone {
    path: OwnedObjectPath,
    connected: bool,
}

async fn paired_hfp_phones(
    connection: &Connection,
    status: &BluetoothStatus,
) -> zbus::Result<Vec<PairedPhone>> {
    type DeviceProperties = HashMap<String, OwnedValue>;
    type DeviceInterfaces = HashMap<String, DeviceProperties>;
    let reply = connection
        .call_method(
            Some("org.bluez"),
            "/",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .await?;
    let objects: HashMap<OwnedObjectPath, DeviceInterfaces> = reply.body().deserialize()?;
    let mut phones = Vec::new();

    for (path, interfaces) in objects {
        let Some(properties) = interfaces.get("org.bluez.Device1") else {
            continue;
        };
        if !is_paired_hfp_device(properties) {
            continue;
        }

        let connected = property_bool(properties, "Connected");
        let name = property_string(properties, "Alias")
            .or_else(|| property_string(properties, "Name"))
            .unwrap_or_else(|| path.to_string());
        status.add_device(path.to_string(), name);
        phones.push(PairedPhone { path, connected });
    }
    Ok(phones)
}

fn is_paired_hfp_device(properties: &HashMap<String, OwnedValue>) -> bool {
    property_bool(properties, "Paired")
        && properties
            .get("UUIDs")
            .and_then(|value| Vec::<String>::try_from(value.clone()).ok())
            .is_some_and(|uuids| {
                uuids
                    .iter()
                    .any(|uuid| uuid.eq_ignore_ascii_case(HFP_AUDIO_GATEWAY_UUID))
            })
}

fn property_bool(properties: &HashMap<String, OwnedValue>, name: &str) -> bool {
    properties
        .get(name)
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

fn property_string(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<String> {
    properties
        .get(name)
        .and_then(|value| String::try_from(value.clone()).ok())
}

fn select_phone(phones: &[PairedPhone]) -> Option<&PairedPhone> {
    let connected: Vec<_> = phones.iter().filter(|phone| phone.connected).collect();
    match connected.as_slice() {
        [phone] => Some(phone),
        [] if phones.len() == 1 => phones.first(),
        [] => None,
        _ => None,
    }
}

fn log_no_phone_selection(phones: &[PairedPhone]) {
    match phones {
        [] => eprintln!("Kein gekoppeltes Handy mit HFP-Audio-Gateway gefunden"),
        _ if phones.iter().filter(|phone| phone.connected).count() > 1 => {
            eprintln!("Mehrere verbundene HFP-Handys gefunden; automatische Auswahl ausgelassen");
        }
        _ => eprintln!("Mehrere gekoppelte HFP-Handys gefunden; automatische Auswahl ausgelassen"),
    }
}

fn spawn_hfp_connection_worker(
    connection: Connection,
    device_path: String,
    reconnect: Arc<Notify>,
) {
    crate::runtime::spawn_critical("HFP-Verbindung", async move {
        let mut retry_delay = Duration::from_secs(5);
        loop {
            match connect_hfp_profile(&connection, &device_path).await {
                Ok(()) => {
                    retry_delay = Duration::from_secs(5);
                    eprintln!("HFP-Verbindung zu {device_path} steht; warte auf Abbruch");
                    reconnect.notified().await;
                    sleep(Duration::from_secs(2)).await;
                }
                Err(error) => {
                    eprintln!(
                        "HFP-Verbindung zu {device_path} fehlgeschlagen: {error}; neuer Versuch in {retry_delay:?}"
                    );
                    sleep(retry_delay).await;
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(60));
                }
            }
        }
    });
}

async fn connect_hfp_profile(connection: &Connection, device_path: &str) -> zbus::Result<()> {
    connection
        .call_method(
            Some("org.bluez"),
            device_path,
            Some("org.bluez.Device1"),
            "ConnectProfile",
            &(HFP_AUDIO_GATEWAY_UUID),
        )
        .await?;
    Ok(())
}
