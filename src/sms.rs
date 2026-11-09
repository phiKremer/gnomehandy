use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use tokio::time::sleep;
use zbus::{
    Connection,
    zvariant::{DynamicType, OwnedObjectPath, OwnedValue, Value},
};

const MAP_UUID: &str = "00001132-0000-1000-8000-00805f9b34fb";

pub(crate) fn spawn_listener(system: Connection, session: Connection, notifications: Connection) {
    crate::runtime::spawn_critical("SMS", async move {
        let mut active_phones = HashSet::new();
        let mut retry_delay = Duration::from_secs(5);
        loop {
            match paired_map_phones(&system).await {
                Ok(phones) => {
                    retry_delay = Duration::from_secs(30);
                    for address in phones {
                        if !active_phones.insert(address.clone()) {
                            continue;
                        }
                        eprintln!("SMS-MAP: Listener für {address} gestartet");
                        let notifications = notifications.clone();
                        let session = session.clone();
                        let phone_address = address.clone();
                        crate::runtime::spawn_critical("SMS", async move {
                            listen_to_phone(&phone_address, session, notifications).await;
                        });
                    }
                    sleep(retry_delay).await;
                }
                Err(error) => {
                    eprintln!(
                        "Gekoppelte MAP-Handys konnten nicht gefunden werden: {error}; erneuter Versuch in {retry_delay:?}"
                    );
                    sleep(retry_delay).await;
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(60));
                }
            }
        }
    });
}

async fn paired_map_phones(system: &Connection) -> zbus::Result<Vec<String>> {
    type Properties = HashMap<String, OwnedValue>;
    type Interfaces = HashMap<String, Properties>;

    let reply = system
        .call_method(
            Some("org.bluez"),
            "/",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .await?;
    let objects: HashMap<OwnedObjectPath, Interfaces> = reply.body().deserialize()?;
    let mut phones = Vec::new();

    for (path, interfaces) in objects {
        let Some(properties) = interfaces.get("org.bluez.Device1") else {
            continue;
        };
        let paired = properties
            .get("Paired")
            .and_then(|value| bool::try_from(value).ok())
            .unwrap_or(false);
        let has_map = properties
            .get("UUIDs")
            .and_then(|value| Vec::<String>::try_from(value.clone()).ok())
            .is_some_and(|uuids| uuids.iter().any(|uuid| uuid.eq_ignore_ascii_case(MAP_UUID)));
        if paired && has_map {
            if let Some(address) = path.as_str().rsplit("dev_").next() {
                phones.push(address.replace('_', ":"));
            }
        }
    }
    Ok(phones)
}

async fn listen_to_phone(address: &str, session: Connection, notifications: Connection) {
    let mut retry_delay = Duration::from_secs(10);
    loop {
        match run_phone_session(address, &session, &notifications).await {
            Ok(()) => retry_delay = Duration::from_secs(10),
            Err(error) => {
                eprintln!(
                    "SMS-MAP zu {address} nicht verfügbar: {error}; neuer Versuch in {retry_delay:?}"
                );
                sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(Duration::from_secs(60));
            }
        }
    }
}

async fn run_phone_session(
    address: &str,
    session: &Connection,
    notifications: &Connection,
) -> zbus::Result<()> {
    let mut options = HashMap::<String, OwnedValue>::new();
    options.insert("Target".into(), OwnedValue::try_from(Value::from("map"))?);
    let reply = session
        .call_method(
            Some("org.bluez.obex"),
            "/org/bluez/obex",
            Some("org.bluez.obex.Client1"),
            "CreateSession",
            &(address, options),
        )
        .await?;
    let session_path: OwnedObjectPath = reply.body().deserialize()?;
    eprintln!("SMS-MAP-Sitzung zu {address} geöffnet ({session_path})");

    let result = poll_phone_session(address, session, notifications, &session_path).await;
    if let Err(error) = session
        .call_method(
            Some("org.bluez.obex"),
            "/org/bluez/obex",
            Some("org.bluez.obex.Client1"),
            "RemoveSession",
            &(&session_path,),
        )
        .await
    {
        eprintln!("SMS-MAP {address}: Sitzung konnte nicht geschlossen werden: {error}");
    }
    result
}

async fn poll_phone_session(
    address: &str,
    session: &Connection,
    notifications: &Connection,
    session_path: &OwnedObjectPath,
) -> zbus::Result<()> {
    call_obex(session, session_path, "SetFolder", &("telecom/msg",)).await?;
    let mut seen = HashSet::new();
    let mut initial_scan = true;
    let mut refresh_enabled = true;

    loop {
        let messages = list_inbox(session, session_path, address, &mut refresh_enabled).await?;
        process_inbox(messages, address, notifications, &mut seen, initial_scan).await;
        initial_scan = false;
        sleep(Duration::from_secs(8)).await;
    }
}

async fn list_inbox(
    connection: &Connection,
    session_path: &OwnedObjectPath,
    address: &str,
    refresh_enabled: &mut bool,
) -> zbus::Result<HashMap<OwnedObjectPath, HashMap<String, OwnedValue>>> {
    if *refresh_enabled {
        if let Err(error) = call_obex(connection, session_path, "UpdateInbox", &()).await {
            if let zbus::Error::MethodError(name, message, _) = &error {
                if name.as_str().ends_with(".NotSupported")
                    || message
                        .as_deref()
                        .is_some_and(|message| message.contains("Bad Request"))
                {
                    *refresh_enabled = false;
                }
            }
            eprintln!(
                "SMS-MAP {address}: optionale Inbox-Aktualisierung abgelehnt: {error}; Nachrichten werden weiterhin direkt abgefragt"
            );
        }
    }
    let reply = call_obex(
        connection,
        session_path,
        "ListMessages",
        &("inbox", HashMap::<String, OwnedValue>::new()),
    )
    .await?;
    let messages: HashMap<OwnedObjectPath, HashMap<String, OwnedValue>> =
        reply.body().deserialize()?;
    eprintln!(
        "SMS-MAP {address}: {} Nachricht(en) im Posteingang",
        messages.len()
    );
    Ok(messages)
}

async fn process_inbox(
    messages: HashMap<OwnedObjectPath, HashMap<String, OwnedValue>>,
    address: &str,
    notifications: &Connection,
    seen: &mut HashSet<String>,
    initial_scan: bool,
) {
    let mut incoming_count = 0;
    let mut metadata_counts = HashMap::<String, usize>::new();
    let mut recent_initial_message: Option<(String, String, String)> = None;
    for (path, properties) in messages {
        record_metadata(&properties, &mut metadata_counts);
        if !is_incoming_sms(&properties) {
            continue;
        }
        incoming_count += 1;
        let is_new = seen.insert(path.to_string());
        if initial_scan {
            recent_initial_message =
                newest_recent_startup_message(recent_initial_message, is_new, &properties);
        } else if is_new {
            let (sender, body) = sms_details(&properties);
            eprintln!("SMS-MAP {address}: neue eingehende Nachricht von {sender}");
            notify_sms(notifications, address, &sender, &body).await;
        }
    }
    if let Some((_, sender, body)) = recent_initial_message {
        eprintln!("SMS-MAP {address}: zeige eine aktuelle SMS nach dem Start an");
        notify_sms(notifications, address, &sender, &body).await;
    }
    eprintln!("SMS-MAP {address}: Nachrichten-Metadaten {metadata_counts:?}");
    eprintln!("SMS-MAP {address}: {incoming_count} eingehende SMS-Einträge erkannt");
}

fn record_metadata(properties: &HashMap<String, OwnedValue>, counts: &mut HashMap<String, usize>) {
    for key in ["Type", "Direction", "Folder"] {
        let value = property_string(properties, key).unwrap_or_else(|| "<fehlt>".to_string());
        *counts.entry(format!("{key}={value}")).or_default() += 1;
    }
}

fn newest_recent_startup_message(
    previous: Option<(String, String, String)>,
    is_new: bool,
    properties: &HashMap<String, OwnedValue>,
) -> Option<(String, String, String)> {
    // MAP session setup can take tens of seconds. Recover the newest SMS
    // received during startup without replaying the old inbox.
    let timestamp = property_string(properties, "Timestamp").unwrap_or_default();
    if !is_new || !is_recent_map_timestamp(&timestamp, Duration::from_secs(5 * 60)) {
        return previous;
    }
    if previous
        .as_ref()
        .is_some_and(|(latest, _, _)| timestamp <= *latest)
    {
        return previous;
    }
    let (sender, body) = sms_details(properties);
    Some((timestamp, sender, body))
}

fn sms_details(properties: &HashMap<String, OwnedValue>) -> (String, String) {
    let sender = property_string(properties, "Sender")
        .or_else(|| property_string(properties, "SenderAddress"))
        .unwrap_or_else(|| "Unbekannte Nummer".to_string());
    let body = property_string(properties, "Subject")
        .filter(|subject| !subject.trim().is_empty())
        .unwrap_or_else(|| "Neue SMS empfangen".to_string());
    (sender, body)
}

async fn notify_sms(notifications: &Connection, address: &str, sender: &str, body: &str) {
    if let Err(error) = crate::service::publish_sms_received(notifications, sender, body).await {
        eprintln!("SMS-MAP {address}: GNOME-Benachrichtigung fehlgeschlagen: {error}");
    } else {
        eprintln!("SMS-MAP {address}: GNOME-Benachrichtigung wurde gesendet");
    }
}

fn is_recent_map_timestamp(timestamp: &str, max_age: Duration) -> bool {
    if timestamp.len() != 15 || timestamp.as_bytes().get(8) != Some(&b'T') {
        return false;
    }
    let parse = |range: std::ops::Range<usize>| timestamp.get(range)?.parse::<i32>().ok();
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        parse(0..4),
        parse(4..6),
        parse(6..8),
        parse(9..11),
        parse(11..13),
        parse(13..15),
    ) else {
        return false;
    };
    let mut local_time: libc::tm = unsafe { std::mem::zeroed() };
    local_time.tm_year = year - 1900;
    local_time.tm_mon = month - 1;
    local_time.tm_mday = day;
    local_time.tm_hour = hour;
    local_time.tm_min = minute;
    local_time.tm_sec = second;
    local_time.tm_isdst = -1;
    let timestamp = unsafe { libc::mktime(&mut local_time) };
    if timestamp < 0 {
        return false;
    }
    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs() as i64,
        Err(_) => return false,
    };
    let age = now - timestamp;
    age >= 0 && age as u64 <= max_age.as_secs()
}

async fn call_obex(
    connection: &Connection,
    path: &OwnedObjectPath,
    method: &str,
    body: &(impl serde::Serialize + DynamicType),
) -> zbus::Result<zbus::Message> {
    connection
        .call_method(
            Some("org.bluez.obex"),
            path,
            Some("org.bluez.obex.MessageAccess1"),
            method,
            body,
        )
        .await
}

fn is_incoming_sms(properties: &HashMap<String, OwnedValue>) -> bool {
    matches!(property_string(properties, "Type").as_deref(), Some("sms-gsm" | "sms-cdma"))
        // BlueZ's ListMessages result on some phones omits Direction even though
        // the Message1 interface defines it. The list is already scoped to inbox.
        && property_string(properties, "Direction")
            .is_none_or(|direction| direction == "incoming")
        && property_string(properties, "Folder").as_deref().is_none_or(|folder| folder.ends_with("inbox"))
}

fn property_string(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    properties
        .get(key)
        .and_then(|value| String::try_from(value.clone()).ok())
}

#[cfg(test)]
mod tests {
    use super::{is_incoming_sms, is_recent_map_timestamp};
    use std::{collections::HashMap, time::Duration};
    use zbus::zvariant::{OwnedValue, Value};

    fn properties(entries: &[(&str, &str)]) -> HashMap<String, OwnedValue> {
        entries
            .iter()
            .map(|(key, value)| {
                (
                    (*key).to_string(),
                    OwnedValue::try_from(Value::from(*value)).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn inbox_sms_without_direction_is_still_recognized() {
        let message = properties(&[("Type", "sms-gsm"), ("Folder", "/telecom/msg/inbox")]);
        assert!(is_incoming_sms(&message));
    }

    #[test]
    fn outgoing_and_non_sms_messages_are_ignored() {
        assert!(!is_incoming_sms(&properties(&[
            ("Type", "sms-gsm"),
            ("Folder", "/telecom/msg/inbox"),
            ("Direction", "outgoing"),
        ])));
        assert!(!is_incoming_sms(&properties(&[
            ("Type", "email"),
            ("Folder", "/telecom/msg/inbox"),
        ])));
    }

    #[test]
    fn invalid_map_timestamps_are_not_treated_as_recent() {
        assert!(!is_recent_map_timestamp(
            "not-a-timestamp",
            Duration::from_secs(300)
        ));
        assert!(!is_recent_map_timestamp(
            "20261341T996199",
            Duration::from_secs(300)
        ));
    }
}
