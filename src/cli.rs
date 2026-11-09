use std::{
    error::Error,
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use tokio::time::sleep;
use zbus::{Connection, connection};

const SERVICE_NAME: &str = "de.pkremer.GnomeHandy";
const OBJECT_PATH: &str = "/de/pkremer/GnomeHandy";
const INTERFACE_NAME: &str = "de.pkremer.GnomeHandy";
const SERVICE_UNIT: &str = "pkremer-gnomehandy.service";
const SERVICE_START_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) async fn dial_uri(uri: &str) -> Result<(), Box<dyn Error>> {
    let number = parse_tel_uri(uri)?;
    let (connection, dev_session_bus) = session_connection().await?;
    let mut service_started = false;
    let deadline = tokio::time::Instant::now() + SERVICE_START_TIMEOUT;

    while tokio::time::Instant::now() < deadline {
        if service_has_owner(&connection).await? {
            let status_reply = connection
                .call_method(
                    Some(SERVICE_NAME),
                    OBJECT_PATH,
                    Some(INTERFACE_NAME),
                    "GetBluetoothStatus",
                    &(),
                )
                .await?;
            let (connected, _) = status_reply.body().deserialize::<(bool, String)>()?;
            if connected {
                connection
                    .call_method(
                        Some(SERVICE_NAME),
                        OBJECT_PATH,
                        Some(INTERFACE_NAME),
                        "DialNumber",
                        &(number,),
                    )
                    .await?;
                return Ok(());
            }
        }

        if !service_started {
            start_service(dev_session_bus)?;
            service_started = true;
        }
        sleep(POLL_INTERVAL).await;
    }

    Err("GNOME Handy-Dienst oder verbundenes Handy ist nicht erreichbar".into())
}

async fn session_connection() -> zbus::Result<(Connection, bool)> {
    if let Some(address) = dev_session_bus_address() {
        if let Ok(builder) = connection::Builder::address(address.as_str()) {
            if let Ok(connection) = builder.build().await {
                return Ok((connection, true));
            }
        }
        eprintln!("Dev-Session-Bus nicht erreichbar; verwende den Host-Session-Bus");
    }
    Ok((Connection::session().await?, false))
}

async fn service_has_owner(connection: &Connection) -> zbus::Result<bool> {
    let reply = connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &(SERVICE_NAME,),
        )
        .await?;
    reply.body().deserialize::<bool>()
}

fn start_service(dev_session_bus: bool) -> Result<(), Box<dyn Error>> {
    if dev_session_bus {
        Command::new(std::env::current_exe()?)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        return Ok(());
    }

    let status = Command::new("systemctl")
        .args(["--user", "start", SERVICE_UNIT])
        .status()?;
    if !status.success() {
        return Err(format!("systemd konnte {SERVICE_UNIT} nicht starten").into());
    }
    Ok(())
}

fn dev_session_bus_address() -> Option<String> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    let marker = PathBuf::from(runtime_dir).join("pkremer-gnomehandy-dev-session-bus");
    std::fs::read_to_string(marker)
        .ok()
        .map(|address| address.trim().to_string())
        .filter(|address| !address.is_empty())
}

fn parse_tel_uri(uri: &str) -> Result<String, Box<dyn Error>> {
    let number = uri
        .strip_prefix("tel:")
        .ok_or("Es werden nur tel:-URIs unterstützt")?
        .split([';', '?'])
        .next()
        .unwrap_or_default();
    let number = percent_decode(number)?;
    let number: String = number
        .chars()
        .filter(|character| !matches!(character, '-' | '.' | '(' | ')' | ' ' | '\t'))
        .collect();
    if number.is_empty() {
        return Err("Die tel:-URI enthält keine Telefonnummer".into());
    }
    if number.len() > 32
        || !number.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_digit() || matches!(byte, b'*' | b'#') || (byte == b'+' && index == 0)
        })
    {
        return Err("Die tel:-URI enthält ein ungültiges Rufnummernformat".into());
    }
    Ok(number)
}

fn percent_decode(value: &str) -> Result<String, Box<dyn Error>> {
    let mut decoded = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = value
                .get(index + 1..index + 3)
                .ok_or("Ungültige Prozentkodierung in tel:-URI")?;
            decoded.push(u8::from_str_radix(hex, 16)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Ok(String::from_utf8(decoded)?)
}

#[cfg(test)]
mod tests {
    use super::{parse_tel_uri, percent_decode};

    #[test]
    fn tel_uri_decodes_and_normalizes_supported_phone_number_formatting() {
        assert_eq!(
            parse_tel_uri("tel:%2B49%20(30)%201234-56;ext=7").unwrap(),
            "+4930123456"
        );
        assert_eq!(parse_tel_uri("tel:12%233?source=contacts").unwrap(), "12#3");
    }

    #[test]
    fn tel_uri_rejects_empty_and_unsupported_values() {
        assert!(parse_tel_uri("tel:").is_err());
        assert!(parse_tel_uri("https://example.org").is_err());
        assert!(parse_tel_uri("tel:%GG").is_err());
        assert!(parse_tel_uri("tel:%FF").is_err());
        assert!(parse_tel_uri("tel:+49123x").is_err());
    }

    #[test]
    fn percent_decode_handles_escaped_utf8_and_rejects_incomplete_sequences() {
        assert_eq!(percent_decode("M%C3%BCnchen").unwrap(), "München");
        assert!(percent_decode("%2").is_err());
        assert!(percent_decode("%GG").is_err());
    }
}
