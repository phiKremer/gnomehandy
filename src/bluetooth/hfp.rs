use std::{
    collections::HashSet,
    fs::File,
    io::{BufRead, BufReader, ErrorKind, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use tokio::sync::Notify;
use tokio::sync::mpsc::Sender;
use zbus::{
    interface,
    zvariant::{OwnedFd, OwnedObjectPath, OwnedValue},
};

use super::BluetoothStatus;

mod audio;
mod call_events;
pub(crate) use audio::ConversationAudio;
pub(crate) use call_events::CallEvent;
use call_events::CallEventParser;

pub(crate) struct HfpHandsFreeProfile {
    connections: Mutex<HashSet<String>>,
    reconnect: Arc<Notify>,
    profile_released: Arc<Notify>,
    status: Arc<BluetoothStatus>,
    call_events: Sender<CallEvent>,
    call_control: Arc<CallController>,
}

#[derive(Default)]
pub(crate) struct CallController {
    writer: Mutex<Option<(String, File)>>,
    pending_outgoing_call: Mutex<Option<String>>,
}

static NEXT_CALL_ID: AtomicU64 = AtomicU64::new(1);

impl CallController {
    fn set_writer(&self, device_path: String, writer: File) {
        if let Ok(mut active) = self.writer.lock() {
            *active = Some((device_path, writer));
        }
    }

    fn clear_writer(&self, device_path: &str) {
        if let Ok(mut active) = self.writer.lock() {
            if active
                .as_ref()
                .is_some_and(|(active_device, _)| active_device == device_path)
            {
                *active = None;
            }
        }
        if let Ok(mut pending) = self.pending_outgoing_call.lock() {
            if pending
                .as_ref()
                .is_some_and(|call_id| call_id.starts_with(&format!("{device_path}#")))
            {
                *pending = None;
            }
        }
    }

    pub(crate) fn answer_call(&self) -> zbus::fdo::Result<()> {
        self.send_command("ATA", "angenommen")
    }

    pub(crate) fn hang_up(&self) -> zbus::fdo::Result<()> {
        self.send_command("AT+CHUP", "beendet")
    }

    pub(crate) fn request_audio_connection(&self) -> zbus::fdo::Result<()> {
        self.send_command("AT+BCC", "angefordert")
    }

    pub(crate) fn send_dtmf(&self, tone: &str) -> zbus::fdo::Result<()> {
        if tone.len() != 1 || !tone.as_bytes()[0].is_ascii_digit() && !"*#ABCD".contains(tone) {
            return Err(zbus::fdo::Error::InvalidArgs(
                "DTMF muss eine Taste von 0–9, *, # oder A–D sein".into(),
            ));
        }
        self.send_command(&format!("AT+VTS={tone}"), "übertragen")
    }

    pub(crate) fn dial_number(&self, number: &str) -> zbus::fdo::Result<String> {
        let number = number.trim();
        if number.is_empty()
            || number.len() > 32
            || !number.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_digit() || matches!(byte, b'*' | b'#') || (byte == b'+' && index == 0)
            })
        {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Telefonnummer darf nur Ziffern, *, # und ein führendes + enthalten".into(),
            ));
        }
        let device_path = self
            .writer
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("HFP-Steuerung ist nicht verfügbar".into()))?
            .as_ref()
            .map(|(device_path, _)| device_path.clone())
            .ok_or_else(|| zbus::fdo::Error::Failed("Kein HFP-Handy verbunden".into()))?;
        let call_id = format!(
            "{}#{}",
            device_path,
            NEXT_CALL_ID.fetch_add(1, Ordering::Relaxed)
        );
        *self.pending_outgoing_call.lock().map_err(|_| {
            zbus::fdo::Error::Failed("HFP-Anrufstatus ist nicht verfügbar".into())
        })? = Some(call_id.clone());
        if let Err(error) = self.send_command(&format!("ATD{number};"), "gewählt") {
            if let Ok(mut pending) = self.pending_outgoing_call.lock() {
                if pending.as_deref() == Some(call_id.as_str()) {
                    *pending = None;
                }
            }
            return Err(error);
        }
        Ok(call_id)
    }

    fn take_pending_outgoing_call(&self) -> Option<String> {
        self.pending_outgoing_call.lock().ok()?.take()
    }

    fn send_command(&self, command: &str, action: &str) -> zbus::fdo::Result<()> {
        let mut active = self
            .writer
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("HFP-Steuerung ist nicht verfügbar".into()))?;
        let Some((device_path, writer)) = active.as_mut() else {
            return Err(zbus::fdo::Error::Failed("Kein HFP-Handy verbunden".into()));
        };

        if !command.starts_with("AT+VTS=") {
            eprintln!("HFP-HF: {command}");
        }
        writer
            .write_all(format!("{command}\r").as_bytes())
            .and_then(|()| writer.flush())
            .map_err(|error| {
                zbus::fdo::Error::Failed(format!(
                    "Anruf auf {device_path} konnte nicht {action} werden: {error}"
                ))
            })
    }
}

impl HfpHandsFreeProfile {
    pub(crate) fn new(
        reconnect: Arc<Notify>,
        profile_released: Arc<Notify>,
        status: Arc<BluetoothStatus>,
        call_events: Sender<CallEvent>,
        call_control: Arc<CallController>,
    ) -> Self {
        Self {
            connections: Mutex::new(HashSet::new()),
            reconnect,
            profile_released,
            status,
            call_events,
            call_control,
        }
    }
}

#[interface(name = "org.bluez.Profile1")]
impl HfpHandsFreeProfile {
    fn release(&self) {
        if let Ok(mut connections) = self.connections.lock() {
            connections.clear();
        }
        self.status.clear();
        self.reconnect.notify_one();
        self.profile_released.notify_one();
        eprintln!("BlueZ HFP-HF-Profil wurde freigegeben");
    }

    fn new_connection(
        &self,
        device: OwnedObjectPath,
        fd: OwnedFd,
        properties: std::collections::HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        let version = properties
            .get("Version")
            .and_then(|value| u16::try_from(value).ok());
        eprintln!("HFP-Verbindung von {device} angenommen (Version: {version:?})");
        self.connections
            .lock()
            .map_err(|_| {
                zbus::fdo::Error::Failed("HFP-Verbindungsstatus ist nicht verfügbar".into())
            })?
            .insert(device.to_string());

        let device_path = device.to_string();
        let reconnect = Arc::clone(&self.reconnect);
        let status = Arc::clone(&self.status);
        let call_events = self.call_events.clone();
        let call_control = Arc::clone(&self.call_control);
        thread::Builder::new()
            .name("gnomehandy-hfp".into())
            .spawn(move || {
                if let Err(error) = run_handsfree_session(
                    fd,
                    &device_path,
                    &status,
                    call_events,
                    Arc::clone(&call_control),
                ) {
                    eprintln!("HFP-Sitzung mit {device_path} fehlgeschlagen: {error}");
                } else {
                    eprintln!("HFP-Sitzung mit {device_path} beendet");
                }
                status.clear_connected_device(&device_path);
                call_control.clear_writer(&device_path);
                reconnect.notify_one();
            })
            .map_err(|error| {
                zbus::fdo::Error::Failed(format!(
                    "HFP-Thread konnte nicht gestartet werden: {error}"
                ))
            })?;
        Ok(())
    }

    fn request_disconnection(&self, device: OwnedObjectPath) -> zbus::fdo::Result<()> {
        self.connections
            .lock()
            .map_err(|_| {
                zbus::fdo::Error::Failed("HFP-Verbindungsstatus ist nicht verfügbar".into())
            })?
            .remove(device.as_str());
        self.status.clear_connected_device(device.as_str());
        self.call_control.clear_writer(device.as_str());
        eprintln!("HFP-Verbindung zu {device} getrennt");
        Ok(())
    }
}

fn run_handsfree_session(
    fd: OwnedFd,
    device_path: &str,
    status: &BluetoothStatus,
    call_events: Sender<CallEvent>,
    call_control: Arc<CallController>,
) -> std::io::Result<()> {
    let owned_fd = std::os::fd::OwnedFd::from(fd);
    use std::os::fd::AsRawFd;
    let raw_fd = owned_fd.as_raw_fd();
    let flags = unsafe { libc::fcntl(raw_fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(raw_fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } == -1
    {
        return Err(std::io::Error::last_os_error());
    }
    let file = std::fs::File::from(owned_fd);
    let mut stream = BufReader::new(file);

    let brsf = hfp_command(&mut stream, "AT+BRSF=5")?;
    if !brsf.iter().any(|line| line.starts_with("+BRSF:")) {
        return Err(std::io::Error::other("AG-Antwort auf BRSF fehlt"));
    }
    eprintln!("HFP-HF/AG-Featureaustausch abgeschlossen: {brsf:?}");

    let cind_test = hfp_command(&mut stream, "AT+CIND=?")?;
    if !cind_test.iter().any(|line| line.starts_with("+CIND:")) {
        return Err(std::io::Error::other("AG-Antwort auf CIND-Test fehlt"));
    }
    let cind_values = hfp_command(&mut stream, "AT+CIND?")?;
    if !cind_values.iter().any(|line| line.starts_with("+CIND:")) {
        return Err(std::io::Error::other("AG-Antwort auf CIND-Abfrage fehlt"));
    }
    hfp_command(&mut stream, "AT+CMER=3,0,0,1")?;
    hfp_command(&mut stream, "AT+CLIP=1")?;
    hfp_command(&mut stream, "AT+CCWA=1")?;
    // HFP requires the hands-free unit to synchronize its current speaker
    // volume and microphone gain when the service-level connection is set up.
    // We currently expose no separate gain control, so report the stream's
    // configured maximum microphone gain to the audio gateway.
    for command in ["AT+VGS=15", "AT+VGM=15"] {
        if let Err(error) = hfp_command(&mut stream, command) {
            eprintln!(
                "HFP-Pegel konnte nicht mit dem Handy synchronisiert werden ({command}): {error}"
            );
        }
    }
    status.set_connected_device(device_path.to_string());
    let writer = stream.get_ref().try_clone()?;
    call_control.set_writer(device_path.to_string(), writer);
    eprintln!("HFP-Service-Level-Verbindung initialisiert");
    let mut call_parser = CallEventParser::new(
        &cind_test,
        &cind_values,
        device_path.to_string(),
        call_events,
        Arc::clone(&call_control),
    );

    // Keep the RFCOMM channel alive and observe subsequent AG notifications.
    let session_result = loop {
        let mut line = String::new();
        match stream.read_line(&mut line) {
            Ok(0) => break Ok(()),
            Ok(_) => {}
            Err(error) => break Err(error),
        }
        let line = line.trim();
        if !line.is_empty() {
            eprintln!("HFP-AG: {line}");
            call_parser.process_line(line);
        }
    };
    call_parser.end_active_call();
    session_result
}

fn hfp_command(
    stream: &mut BufReader<std::fs::File>,
    command: &str,
) -> std::io::Result<Vec<String>> {
    stream
        .get_mut()
        .write_all(format!("{command}\r").as_bytes())?;
    stream.get_mut().flush()?;
    eprintln!("HFP-HF: {command}");

    let mut responses = Vec::new();
    loop {
        let mut line = String::new();
        if stream.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                ErrorKind::UnexpectedEof,
                "AG hat RFCOMM getrennt",
            ));
        }
        let line = line.trim().to_string();
        if line.is_empty() || line == command {
            continue;
        }
        eprintln!("HFP-AG: {line}");
        if line == "OK" {
            return Ok(responses);
        }
        if line == "ERROR" || line.starts_with("+CME ERROR") {
            return Err(std::io::Error::other(format!(
                "AG lehnte {command} ab: {line}"
            )));
        }
        responses.push(line);
    }
}
