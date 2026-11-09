use crate::audio::{self, AudioRoute, InputRoute};
use std::{
    ffi::{CString, c_char, c_int, c_void},
    fs::File,
    io::{Read, Write},
    mem,
    os::fd::{FromRawFd, RawFd},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

const SCO_PROTOCOL: c_int = 2;
const SOL_BLUETOOTH: c_int = 274;
const BT_VOICE: c_int = 11;
const BT_VOICE_CVSD_16BIT: u16 = 0x0060;
const BT_SNDMTU: c_int = 12;
const BT_RCVMTU: c_int = 13;
const SO_SNDTIMEO: c_int = 21;
const SCO_CONNECT_TIMEOUT_MS: libc::c_int = 5_000;
const SAMPLE_RATE: u32 = 8_000;
const CHANNELS: u8 = 1;
// CVSD links commonly use 48-byte SCO payloads. The socket MTU is only a
// maximum (our adapter reports 96), while the negotiated link TX length can
// be smaller; keep each packet within the standard 48-byte CVSD payload.
const CVSD_PACKET_SIZE: usize = 48;
const PA_SAMPLE_S16LE: c_int = 3;
const PA_STREAM_PLAYBACK: c_int = 1;
const PA_STREAM_RECORD: c_int = 2;

#[repr(C)]
struct PaSampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

#[repr(C)]
struct PaBufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

enum PaSimple {}

#[link(name = "pulse-simple")]
unsafe extern "C" {
    fn pa_simple_new(
        server: *const c_char,
        name: *const c_char,
        direction: c_int,
        device: *const c_char,
        stream_name: *const c_char,
        sample_spec: *const PaSampleSpec,
        channel_map: *const c_void,
        buffer_attr: *const PaBufferAttr,
        error: *mut c_int,
    ) -> *mut PaSimple;
    fn pa_simple_read(
        stream: *mut PaSimple,
        data: *mut c_void,
        bytes: usize,
        error: *mut c_int,
    ) -> c_int;
    fn pa_simple_write(
        stream: *mut PaSimple,
        data: *const c_void,
        bytes: usize,
        error: *mut c_int,
    ) -> c_int;
    fn pa_simple_free(stream: *mut PaSimple);
}

#[repr(C)]
struct SockAddrSco {
    family: u16,
    address: [u8; 6],
}

#[repr(C)]
struct TimeVal {
    seconds: libc::time_t,
    microseconds: libc::suseconds_t,
}

struct PulseStream(*mut PaSimple);
unsafe impl Send for PulseStream {}

impl PulseStream {
    fn open(direction: c_int, device: Option<&str>, name: &str) -> Result<Self, String> {
        let application = CString::new("GNOME Handy").map_err(|error| error.to_string())?;
        let stream_name = CString::new(name).map_err(|error| error.to_string())?;
        let device = device
            .map(CString::new)
            .transpose()
            .map_err(|error| error.to_string())?;
        let spec = PaSampleSpec {
            format: PA_SAMPLE_S16LE,
            rate: SAMPLE_RATE,
            channels: CHANNELS,
        };
        // The simple API otherwise lets PulseAudio choose a large capture
        // buffer. Keep microphone-to-SCO latency bounded to short voice frames.
        let record_buffer = PaBufferAttr {
            maxlength: 3_200,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: 160,
        };
        let mut error = 0;
        // The PulseAudio simple API owns one independent stream per direction.
        let stream = unsafe {
            pa_simple_new(
                std::ptr::null(),
                application.as_ptr(),
                direction,
                device
                    .as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
                stream_name.as_ptr(),
                &spec,
                std::ptr::null(),
                if direction == PA_STREAM_RECORD {
                    &record_buffer
                } else {
                    std::ptr::null()
                },
                &mut error,
            )
        };
        if stream.is_null() {
            return Err(format!(
                "PulseAudio konnte {name} nicht öffnen (Fehler {error})"
            ));
        }
        Ok(Self(stream))
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<(), String> {
        let mut error = 0;
        let result =
            unsafe { pa_simple_read(self.0, buffer.as_mut_ptr().cast(), buffer.len(), &mut error) };
        if result < 0 {
            Err(format!(
                "PulseAudio-Aufnahme fehlgeschlagen (Fehler {error})"
            ))
        } else {
            Ok(())
        }
    }

    fn write(&mut self, buffer: &[u8]) -> Result<(), String> {
        let mut error = 0;
        let result =
            unsafe { pa_simple_write(self.0, buffer.as_ptr().cast(), buffer.len(), &mut error) };
        if result < 0 {
            Err(format!(
                "PulseAudio-Wiedergabe fehlgeschlagen (Fehler {error})"
            ))
        } else {
            Ok(())
        }
    }
}

impl Drop for PulseStream {
    fn drop(&mut self) {
        unsafe { pa_simple_free(self.0) };
    }
}

struct ActiveConversation {
    call_id: String,
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
    output_route: Option<AudioRoute>,
    input_route: Option<InputRoute>,
}

#[derive(Default)]
pub(crate) struct ConversationAudio {
    active: Mutex<Option<ActiveConversation>>,
}

impl ConversationAudio {
    pub(crate) fn start(
        &self,
        call_id: &str,
        device_address: &str,
        output_selection: &str,
        input_selection: &str,
    ) -> Result<(), String> {
        self.stop();
        let output_route = resolve_output(output_selection)?;
        let input_route = resolve_input(input_selection)?;
        activate_ports(output_route.as_ref(), input_route.as_ref())?;
        let prepared = match prepare_conversation_audio(device_address, &output_route, &input_route)
        {
            Ok(prepared) => prepared,
            Err(error) => {
                restore_ports(output_route.as_ref(), input_route.as_ref());
                return Err(error);
            }
        };
        let (stop, workers) = match spawn_audio_workers(prepared) {
            Ok(workers) => workers,
            Err(error) => {
                restore_ports(output_route.as_ref(), input_route.as_ref());
                return Err(error);
            }
        };

        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(ActiveConversation {
                call_id: call_id.into(),
                stop,
                workers,
                output_route,
                input_route,
            });
        eprintln!("Gesprächsaudio über SCO/eSCO gestartet für {device_address}");
        Ok(())
    }

    pub(crate) fn stop(&self) {
        let active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(active) = active {
            active.stop.store(true, Ordering::Release);
            for worker in active.workers {
                let _ = worker.join();
            }
            restore_ports(active.output_route.as_ref(), active.input_route.as_ref());
            eprintln!("Gesprächsaudio beendet für {}", active.call_id);
        }
    }

    pub(crate) fn stop_call(&self, call_id: &str) {
        let is_active_call = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|active| active.call_id == call_id);
        if is_active_call {
            self.stop();
        }
    }
}

impl Drop for ConversationAudio {
    fn drop(&mut self) {
        self.stop();
    }
}

struct PreparedConversationAudio {
    sco: File,
    output: PulseStream,
    input: PulseStream,
    send_mtu: usize,
    receive_mtu: usize,
}

fn prepare_conversation_audio(
    device_address: &str,
    output_route: &Option<AudioRoute>,
    input_route: &Option<InputRoute>,
) -> Result<PreparedConversationAudio, String> {
    let (sco, send_mtu, receive_mtu) = connect_sco(device_address)?;
    let output = PulseStream::open(
        PA_STREAM_PLAYBACK,
        output_route.as_ref().map(|route| route.sink_name.as_str()),
        "GNOME Handy Gesprächsausgabe",
    )?;
    let input = PulseStream::open(
        PA_STREAM_RECORD,
        input_route.as_ref().map(|route| route.source_name.as_str()),
        "GNOME Handy Mikrofon",
    )?;
    Ok(PreparedConversationAudio {
        sco,
        output,
        input,
        send_mtu,
        receive_mtu,
    })
}

fn spawn_audio_workers(
    prepared: PreparedConversationAudio,
) -> Result<(Arc<AtomicBool>, Vec<JoinHandle<()>>), String> {
    let receive_socket = prepared
        .sco
        .try_clone()
        .map_err(|error| format!("SCO-Socket konnte nicht dupliziert werden: {error}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let receive_stop = Arc::clone(&stop);
    let receive_mtu = prepared.receive_mtu;
    let receive = thread::Builder::new()
        .name("gnomehandy-sco-playback".into())
        .spawn(move || {
            receive_to_speakers(receive_socket, prepared.output, receive_stop, receive_mtu)
        })
        .map_err(|error| format!("SCO-Ausgabethread konnte nicht gestartet werden: {error}"))?;

    let transmit_stop = Arc::clone(&stop);
    let transmit_mtu = prepared.send_mtu;
    match thread::Builder::new()
        .name("gnomehandy-sco-capture".into())
        .spawn(move || microphone_to_sco(prepared.sco, prepared.input, transmit_stop, transmit_mtu))
    {
        Ok(transmit) => Ok((stop, vec![receive, transmit])),
        Err(error) => {
            stop.store(true, Ordering::Release);
            let _ = receive.join();
            Err(format!(
                "SCO-Aufnahmethread konnte nicht gestartet werden: {error}"
            ))
        }
    }
}

fn resolve_output(selection: &str) -> Result<Option<AudioRoute>, String> {
    if selection.is_empty() {
        return Ok(None);
    }
    audio::output_routes(selection)
        .map_err(|error| error.to_string())?
        .into_iter()
        .next()
        .map(Some)
        .ok_or_else(|| "Gesprächsausgabe wurde nicht gefunden".into())
}

fn resolve_input(selection: &str) -> Result<Option<InputRoute>, String> {
    if selection.is_empty() {
        return Ok(None);
    }
    audio::input_route(selection)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn activate_ports(output: Option<&AudioRoute>, input: Option<&InputRoute>) -> Result<(), String> {
    if let Some(route) = output {
        if let Some(port) = route.port_name.as_deref() {
            audio::set_output_port(route.sink_index, port)?;
        }
    }
    if let Some(route) = input {
        if let Some(port) = route.port_name.as_deref() {
            if let Err(error) = audio::set_input_port(route.source_index, port) {
                restore_ports(output, None);
                return Err(error);
            }
        }
    }
    Ok(())
}

fn restore_ports(output: Option<&AudioRoute>, input: Option<&InputRoute>) {
    if let Some(route) = output {
        if let Some(port) = route.active_port.as_deref() {
            let _ = audio::set_output_port(route.sink_index, port);
        }
    }
    if let Some(route) = input {
        if let Some(port) = route.active_port.as_deref() {
            let _ = audio::set_input_port(route.source_index, port);
        }
    }
}

fn connect_sco(address: &str) -> Result<(File, usize, usize), String> {
    let bytes = parse_bluetooth_address(address)?;
    let fd = unsafe {
        libc::socket(
            libc::AF_BLUETOOTH,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            SCO_PROTOCOL,
        )
    };
    if fd < 0 {
        return Err(format!(
            "SCO-Socket konnte nicht geöffnet werden: {}",
            std::io::Error::last_os_error()
        ));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let voice: u16 = BT_VOICE_CVSD_16BIT;
    if unsafe {
        libc::setsockopt(
            fd,
            SOL_BLUETOOTH,
            BT_VOICE,
            (&voice as *const u16).cast(),
            mem::size_of_val(&voice) as _,
        )
    } < 0
    {
        return Err(format!(
            "CVSD-Audioformat konnte nicht gesetzt werden: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut remote = SockAddrSco {
        family: libc::AF_BLUETOOTH as u16,
        address: [0; 6],
    };
    for (destination, source) in remote.address.iter_mut().zip(bytes.iter().rev()) {
        *destination = *source;
    }
    connect_with_timeout(fd, &remote)?;
    let send_mtu = socket_mtu(fd, BT_SNDMTU)?;
    let receive_mtu = socket_mtu(fd, BT_RCVMTU)?;
    if send_mtu < 2 || receive_mtu < 2 || send_mtu % 2 != 0 || receive_mtu % 2 != 0 {
        return Err(format!(
            "Bluetooth lieferte ungültige SCO-MTUs: Senden {send_mtu}, Empfangen {receive_mtu}"
        ));
    }
    eprintln!("SCO verbunden; Paketgrößen: Senden {send_mtu} B, Empfangen {receive_mtu} B");
    set_socket_timeout(fd, libc::SO_RCVTIMEO)?;
    set_socket_timeout(fd, SO_SNDTIMEO)?;
    Ok((file, send_mtu, receive_mtu))
}

fn set_socket_timeout(fd: RawFd, option: c_int) -> Result<(), String> {
    let timeout = TimeVal {
        seconds: 0,
        microseconds: 200_000,
    };
    // SAFETY: timeout is a valid timeval and setsockopt reads exactly its size.
    if unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            (&timeout as *const TimeVal).cast(),
            mem::size_of_val(&timeout) as _,
        )
    } < 0
    {
        return Err(format!(
            "SCO-I/O-Zeitlimit konnte nicht gesetzt werden: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn parse_bluetooth_address(address: &str) -> Result<Vec<u8>, String> {
    let bytes = address
        .split(':')
        .map(|part| u8::from_str_radix(part, 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Ungültige Bluetooth-Adresse: {error}"))?;
    if bytes.len() != 6 {
        return Err("Bluetooth-Adresse muss aus sechs Bytes bestehen".into());
    }
    Ok(bytes)
}

fn connect_with_timeout(fd: RawFd, remote: &SockAddrSco) -> Result<(), String> {
    // A blocking SCO connect can stall call setup indefinitely when BlueZ or
    // the phone has gone away. Use a bounded nonblocking connect instead.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(format!(
            "SCO-Socket konnte nicht vorbereitet werden: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: remote is a live sockaddr_sco-compatible repr(C) value and the
    // length is its exact size. The fd remains owned by the caller.
    let result = unsafe {
        libc::connect(
            fd,
            (remote as *const SockAddrSco).cast(),
            mem::size_of_val(remote) as _,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock
            && error.raw_os_error() != Some(libc::EINPROGRESS)
        {
            return Err(format!("SCO-Verbindung zum Handy fehlgeschlagen: {error}"));
        }
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: descriptor points to one initialized pollfd for this call.
        let ready = unsafe { libc::poll(&mut descriptor, 1, SCO_CONNECT_TIMEOUT_MS) };
        if ready == 0 {
            return Err("SCO-Verbindung zum Handy: Zeitüberschreitung nach 5 Sekunden".into());
        }
        if ready < 0 {
            return Err(format!(
                "SCO-Verbindung konnte nicht abgewartet werden: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut socket_error: libc::c_int = 0;
        let mut length = mem::size_of_val(&socket_error) as libc::socklen_t;
        // SAFETY: socket_error and length are valid writable output buffers.
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut socket_error as *mut libc::c_int).cast(),
                &mut length,
            )
        } < 0
        {
            return Err(format!(
                "SCO-Verbindungsstatus konnte nicht gelesen werden: {}",
                std::io::Error::last_os_error()
            ));
        }
        if socket_error != 0 {
            return Err(format!(
                "SCO-Verbindung zum Handy fehlgeschlagen: {}",
                std::io::Error::from_raw_os_error(socket_error)
            ));
        }
    }
    // Restore blocking mode; the configured socket I/O timeouts bound worker
    // shutdown without changing the packet-oriented SCO read/write behavior.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags) } < 0 {
        return Err(format!(
            "SCO-Socket konnte nicht zurückgesetzt werden: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn socket_mtu(fd: RawFd, option: c_int) -> Result<usize, String> {
    let mut mtu: u32 = 0;
    let mut length = mem::size_of_val(&mtu) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            SOL_BLUETOOTH,
            option,
            (&mut mtu as *mut u32).cast(),
            &mut length,
        )
    } < 0
    {
        return Err(format!(
            "SCO-Paketgröße konnte nicht abgefragt werden: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(mtu as usize)
}

fn receive_to_speakers(
    mut socket: File,
    mut output: PulseStream,
    stop: Arc<AtomicBool>,
    receive_mtu: usize,
) {
    let mut buffer = vec![0_u8; receive_mtu.max(256)];
    let mut packet_count = 0_u64;
    while !stop.load(Ordering::Acquire) {
        match socket.read(&mut buffer) {
            Ok(0) => break,
            Ok(length) => {
                if let Err(error) = output.write(&buffer[..length]) {
                    eprintln!("Bluetooth-Ton konnte nicht ausgegeben werden: {error}");
                    break;
                }
                packet_count += 1;
                if packet_count == 1 {
                    eprintln!("Erstes SCO-Audiopaket vom Handy am Host ausgegeben ({length} B)");
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(error) => {
                eprintln!("SCO-Audioempfang fehlgeschlagen: {error}");
                break;
            }
        }
    }
}

fn microphone_to_sco(
    mut socket: File,
    mut input: PulseStream,
    stop: Arc<AtomicBool>,
    send_mtu: usize,
) {
    let packet_size = send_mtu.min(CVSD_PACKET_SIZE);
    let mut buffer = vec![0_u8; packet_size];
    while !stop.load(Ordering::Acquire) {
        if let Err(error) = input.read(&mut buffer) {
            eprintln!("Mikrofonaufnahme für den Anruf fehlgeschlagen: {error}");
            break;
        }
        match socket.write(&buffer) {
            Ok(length) if length == packet_size => {}
            Ok(length) => {
                eprintln!(
                    "SCO akzeptierte unvollständiges Mikrofonpaket ({length}/{packet_size} B)"
                );
                break;
            }
            Err(error) => {
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) {
                    eprintln!(
                        "SCO-Mikrofonpaket wurde nicht rechtzeitig angenommen; aktuelles Paket verworfen"
                    );
                    continue;
                }
                eprintln!("Mikrofonaudio konnte nicht per SCO gesendet werden: {error}");
                break;
            }
        }
    }
}
