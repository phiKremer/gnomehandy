use std::{
    cell::Cell,
    fs::File,
    io::ErrorKind,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use libpulse_binding as pulse;
use pulse::{
    context::{Context, FlagSet as ContextFlags, State as ContextState},
    mainloop::standard::{IterateResult, Mainloop},
    operation::State as OperationState,
    proplist::Proplist,
    sample::{Format, Spec},
    stream::{FlagSet as StreamFlags, SeekMode, Stream},
};
use symphonia::{
    core::{
        audio::SampleBuffer,
        codecs::DecoderOptions,
        errors::Error as DecodeError,
        formats::FormatOptions,
        io::{MediaSourceStream, MediaSourceStreamOptions},
        meta::MetadataOptions,
        probe::Hint,
    },
    default::{get_codecs, get_probe},
};

use crate::audio::AudioRoute;

const SAMPLE_RATE: u32 = 48_000;
const REPEAT_INTERVAL: Duration = Duration::from_secs(5);
const FRAMES_PER_CHUNK: usize = 480;
const FADE_OUT_FRAMES: usize = SAMPLE_RATE as usize / 12;
const RING_BUFFER_TARGET_BYTES: u32 = 19_200;
const PULSE_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);
const PULSE_POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_DECODED_SAMPLES: usize = 16 * 1024 * 1024;

struct ActiveRingtone {
    stop: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}

#[derive(Default)]
pub(crate) struct RingtonePlayer {
    active: Mutex<Option<ActiveRingtone>>,
}

impl RingtonePlayer {
    pub(crate) fn start(&self, path: PathBuf, routes: Vec<AudioRoute>) {
        self.stop();

        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("gnomehandy-ringtone".into())
            .spawn(move || {
                let audio = match decode_audio_file(&path) {
                    Ok(audio) => audio,
                    Err(error) => {
                        eprintln!("Klingelton {path:?} konnte nicht dekodiert werden: {error}");
                        return;
                    }
                };
                let route_groups = group_routes(routes);
                if route_groups.is_empty() {
                    eprintln!("Für den Klingelton wurde kein erreichbarer Audioausgang gefunden");
                    return;
                }

                let players = route_groups
                    .into_iter()
                    .map(|(sink, routes)| {
                        let stop = Arc::clone(&worker_stop);
                        let audio = Arc::clone(&audio);
                        thread::Builder::new()
                            .name("gnomehandy-ring-sink".into())
                            .spawn(move || play_repeatedly(&sink, &routes, &audio, &stop))
                    })
                    .collect::<std::io::Result<Vec<_>>>();

                let Ok(players) = players else {
                    eprintln!("Klingelton-Ausgabethread konnte nicht gestartet werden");
                    worker_stop.store(true, Ordering::Release);
                    return;
                };
                while !worker_stop.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(100));
                }
                for player in players {
                    let _ = player.join();
                }
            });

        match worker {
            Ok(worker) => {
                *self
                    .active
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(ActiveRingtone { stop, worker });
            }
            Err(error) => eprintln!("Klingelton-Thread konnte nicht gestartet werden: {error}"),
        }
    }

    pub(crate) fn stop(&self) {
        let active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(active) = active {
            active.stop.store(true, Ordering::Release);
            let _ = active.worker.join();
        }
    }
}

impl Drop for RingtonePlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn group_routes(routes: Vec<AudioRoute>) -> Vec<(String, Vec<AudioRoute>)> {
    let mut groups = std::collections::BTreeMap::<String, Vec<AudioRoute>>::new();
    for route in routes {
        groups
            .entry(route.sink_name.clone())
            .or_default()
            .push(route);
    }
    groups.into_iter().collect()
}

fn play_repeatedly(
    sink_name: &str,
    routes: &[AudioRoute],
    audio: &DecodedAudio,
    stop: &AtomicBool,
) {
    let (mut mainloop, mut context) = match open_pulse_context(sink_name) {
        Ok(context) => context,
        Err(error) => {
            eprintln!("PulseAudio-Kontext für {sink_name} fehlgeschlagen: {error}");
            return;
        }
    };
    let mut stream = match open_playback_stream(sink_name, &mut mainloop, &mut context) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("PulseAudio-Stream zu {sink_name} fehlgeschlagen: {error}");
            return;
        }
    };

    play_routes_until_stopped(
        sink_name,
        routes,
        audio,
        stop,
        &mut mainloop,
        &mut context,
        &mut stream,
    );
    let _ = stream.disconnect();
    context.disconnect();
}

fn open_pulse_context(sink_name: &str) -> Result<(Mainloop, Context), String> {
    let mut mainloop = Mainloop::new().ok_or_else(|| {
        format!("PulseAudio-Mainloop für {sink_name} konnte nicht erstellt werden")
    })?;
    let mut properties =
        Proplist::new().ok_or("PulseAudio-Proplist konnte nicht erstellt werden")?;
    properties
        .set_str(
            pulse::proplist::properties::APPLICATION_NAME,
            "GNOME Handy Klingelton",
        )
        .map_err(|error| {
            format!("PulseAudio-Anwendungsname konnte nicht gesetzt werden: {error:?}")
        })?;
    let mut context = Context::new_with_proplist(&mainloop, "GNOME Handy Klingelton", &properties)
        .ok_or("PulseAudio-Kontext konnte nicht erstellt werden")?;
    context
        .connect(None, ContextFlags::NOFLAGS, None)
        .map_err(|error| {
            format!("PulseAudio-Verbindung zu {sink_name} fehlgeschlagen: {error:?}")
        })?;
    wait_for_context(&mut mainloop, &context)?;
    Ok((mainloop, context))
}

fn open_playback_stream(
    sink_name: &str,
    mainloop: &mut Mainloop,
    context: &mut Context,
) -> Result<Stream, String> {
    let format = if cfg!(target_endian = "little") {
        Format::F32le
    } else {
        Format::F32be
    };
    let spec = Spec {
        format,
        channels: 2,
        rate: SAMPLE_RATE,
    };
    if !spec.is_valid() {
        return Err("Ungültiges PulseAudio-Sampleformat für Klingelton".into());
    }
    let mut stream = Stream::new(context, "Eingehender Anruf", &spec, None)
        .ok_or("PulseAudio-Klingeltonstream konnte nicht erstellt werden")?;
    let buffer_attr = pulse::def::BufferAttr {
        maxlength: u32::MAX,
        tlength: RING_BUFFER_TARGET_BYTES,
        prebuf: u32::MAX,
        minreq: (FRAMES_PER_CHUNK * 2 * std::mem::size_of::<f32>()) as u32,
        fragsize: u32::MAX,
    };
    stream
        .connect_playback(
            Some(sink_name),
            Some(&buffer_attr),
            StreamFlags::NOFLAGS,
            None,
            None,
        )
        .map_err(|error| {
            format!(
                "Klingelton konnte nicht an Audioausgang {sink_name} geroutet werden: {error:?}"
            )
        })?;
    wait_for_stream(mainloop, &stream)?;
    Ok(stream)
}

fn play_routes_until_stopped(
    sink_name: &str,
    routes: &[AudioRoute],
    audio: &DecodedAudio,
    stop: &AtomicBool,
    mainloop: &mut Mainloop,
    context: &mut Context,
    stream: &mut Stream,
) {
    while !stop.load(Ordering::Acquire) {
        for route in routes {
            if stop.load(Ordering::Acquire) {
                break;
            }
            if let Some(port) = route.port_name.as_deref() {
                if route.active_port.as_deref() != Some(port) {
                    if let Err(error) = set_sink_port(mainloop, context, route.sink_index, port) {
                        eprintln!(
                            "Audioport {port} konnte für den Klingelton nicht aktiviert werden: {error}"
                        );
                        continue;
                    }
                }
            }

            let playback_result = play_once(mainloop, stream, &audio.samples, stop);

            if let Some(port) = route.active_port.as_deref() {
                if route.port_name.as_deref() != Some(port) {
                    if let Err(error) = set_sink_port(mainloop, context, route.sink_index, port) {
                        eprintln!(
                            "Vorheriger Audioport {port} konnte nicht wiederhergestellt werden: {error}"
                        );
                    }
                }
            }
            if let Err(error) = playback_result {
                eprintln!("Klingelton konnte auf {sink_name} nicht abgespielt werden: {error}");
                break;
            }
        }

        let elapsed = std::time::Instant::now();
        while !stop.load(Ordering::Acquire) && elapsed.elapsed() < REPEAT_INTERVAL {
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn play_once(
    mainloop: &mut Mainloop,
    stream: &mut Stream,
    samples: &[f32],
    stop: &AtomicBool,
) -> Result<(), String> {
    let total_frames = samples.len() / 2;
    let mut frame = 0;
    while frame < total_frames {
        if stop.load(Ordering::Acquire) {
            if frame > 0 {
                let fade_end = (frame + FADE_OUT_FRAMES).min(total_frames);
                let fade_frames = fade_end - frame;
                let mut fade = Vec::with_capacity(FADE_OUT_FRAMES * 2);
                for fade_frame in 0..FADE_OUT_FRAMES {
                    let gain = if fade_frame < fade_frames {
                        1.0 - fade_frame as f32 / FADE_OUT_FRAMES as f32
                    } else {
                        0.0
                    };
                    if fade_frame < fade_frames {
                        let sample_index = (frame + fade_frame) * 2;
                        fade.push(samples[sample_index] * gain);
                        fade.push(samples[sample_index + 1] * gain);
                    } else {
                        fade.push(0.0);
                        fade.push(0.0);
                    }
                }
                write_audio_chunk(mainloop, stream, &fade)?;
            }
            return drain_stream(mainloop, stream);
        }

        let end_frame = (frame + FRAMES_PER_CHUNK).min(total_frames);
        let start_sample = frame * 2;
        let end_sample = end_frame * 2;
        write_audio_chunk(mainloop, stream, &samples[start_sample..end_sample])?;
        frame = end_frame;
    }

    drain_stream(mainloop, stream)
}

fn write_audio_chunk(
    mainloop: &mut Mainloop,
    stream: &mut Stream,
    samples: &[f32],
) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(samples));
    for sample in samples {
        bytes.extend_from_slice(&sample.to_ne_bytes());
    }

    let deadline = Instant::now() + PULSE_OPERATION_TIMEOUT;
    while stream.writable_size().unwrap_or(0) < bytes.len() {
        if Instant::now() >= deadline {
            return Err("Zeitüberschreitung beim Schreiben des Klingeltons".into());
        }
        if matches!(
            mainloop.iterate(false),
            IterateResult::Err(_) | IterateResult::Quit(_)
        ) {
            return Err("PulseAudio-Mainloop ist während des Schreibens fehlgeschlagen".into());
        }
        thread::sleep(PULSE_POLL_INTERVAL);
    }
    stream
        .write_copy(&bytes, 0, SeekMode::Relative)
        .map_err(|error| format!("PulseAudio-Schreibfehler: {error:?}"))
}

fn drain_stream(mainloop: &mut Mainloop, stream: &mut Stream) -> Result<(), String> {
    let drained = Rc::new(Cell::new(None));
    let drain_state = Rc::clone(&drained);
    let _drain = stream.drain(Some(Box::new(move |success| {
        drain_state.set(Some(success))
    })));
    let deadline = Instant::now() + PULSE_OPERATION_TIMEOUT;
    while drained.get().is_none() {
        if Instant::now() >= deadline {
            return Err("Zeitüberschreitung beim Ausspielen des Klingeltons".into());
        }
        if matches!(
            mainloop.iterate(false),
            IterateResult::Err(_) | IterateResult::Quit(_)
        ) {
            return Err("PulseAudio-Mainloop ist während des Abspielens fehlgeschlagen".into());
        }
        thread::sleep(PULSE_POLL_INTERVAL);
    }
    if drained.get() == Some(true) {
        Ok(())
    } else {
        Err("PulseAudio konnte den Klingelton nicht vollständig ausspielen".into())
    }
}

fn set_sink_port(
    mainloop: &mut Mainloop,
    context: &mut Context,
    sink_index: u32,
    port: &str,
) -> Result<(), String> {
    let completed = Rc::new(Cell::new(None));
    let callback_state = Rc::clone(&completed);
    let operation = context.introspect().set_sink_port_by_index(
        sink_index,
        port,
        Some(Box::new(move |success| callback_state.set(Some(success)))),
    );
    let deadline = Instant::now() + PULSE_OPERATION_TIMEOUT;
    while operation.get_state() == OperationState::Running {
        if Instant::now() >= deadline {
            return Err("Zeitüberschreitung beim PulseAudio-Portwechsel".into());
        }
        if matches!(
            mainloop.iterate(false),
            IterateResult::Err(_) | IterateResult::Quit(_)
        ) {
            return Err("PulseAudio-Portwechsel ist fehlgeschlagen".into());
        }
        thread::sleep(PULSE_POLL_INTERVAL);
    }
    if completed.get() == Some(true) {
        Ok(())
    } else {
        Err(format!("PulseAudio lehnte den Port {port} ab"))
    }
}

fn wait_for_context(mainloop: &mut Mainloop, context: &Context) -> Result<(), String> {
    let deadline = Instant::now() + PULSE_OPERATION_TIMEOUT;
    while context.get_state() != ContextState::Ready {
        if matches!(
            context.get_state(),
            ContextState::Failed | ContextState::Terminated
        ) {
            return Err("PulseAudio/PipeWire ist nicht erreichbar".into());
        }
        if Instant::now() >= deadline {
            return Err("Zeitüberschreitung beim Verbinden mit PulseAudio/PipeWire".into());
        }
        if matches!(
            mainloop.iterate(false),
            IterateResult::Err(_) | IterateResult::Quit(_)
        ) {
            return Err("PulseAudio-Mainloop ist fehlgeschlagen".into());
        }
        thread::sleep(PULSE_POLL_INTERVAL);
    }
    Ok(())
}

fn wait_for_stream(mainloop: &mut Mainloop, stream: &Stream) -> Result<(), String> {
    let deadline = Instant::now() + PULSE_OPERATION_TIMEOUT;
    while stream.get_state() != pulse::stream::State::Ready {
        if matches!(
            stream.get_state(),
            pulse::stream::State::Failed | pulse::stream::State::Terminated
        ) {
            return Err("PulseAudio-Stream konnte nicht gestartet werden".into());
        }
        if Instant::now() >= deadline {
            return Err("Zeitüberschreitung beim Öffnen des PulseAudio-Streams".into());
        }
        if matches!(
            mainloop.iterate(false),
            IterateResult::Err(_) | IterateResult::Quit(_)
        ) {
            return Err("PulseAudio-Mainloop ist fehlgeschlagen".into());
        }
        thread::sleep(PULSE_POLL_INTERVAL);
    }
    Ok(())
}

struct DecodedAudio {
    samples: Vec<f32>,
}

fn decode_audio_file(path: &PathBuf) -> Result<Arc<DecodedAudio>, String> {
    let source = File::open(path).map_err(|error| error.to_string())?;
    let media_source =
        MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
    let probed = get_probe()
        .format(
            &Hint::new(),
            media_source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|error| error.to_string())?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| "Audiodatei enthält keine Tonspur".to_string())?;
    let track_id = track.id;
    let mut decoder = get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|error| error.to_string())?;
    let mut source_rate = None;
    let mut source_channels = None;
    let mut samples = Vec::<f32>::new();

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(DecodeError::IoError(error)) if error.kind() == ErrorKind::UnexpectedEof => break,
            Err(DecodeError::ResetRequired) => {
                return Err("Audiodatei erfordert einen Decoder-Reset".into());
            }
            Err(error) => return Err(error.to_string()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = decoder.decode(&packet).map_err(|error| error.to_string())?;
        let spec = *decoded.spec();
        if spec.rate == 0 || spec.channels.count() == 0 {
            return Err("Audiodatei enthält ein ungültiges Sampleformat".into());
        }
        source_rate.get_or_insert(spec.rate);
        source_channels.get_or_insert(spec.channels.count());
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        if samples.len().saturating_add(buffer.samples().len()) > MAX_DECODED_SAMPLES {
            return Err("Klingelton überschreitet die maximale dekodierte Größe von 64 MiB".into());
        }
        samples.extend_from_slice(buffer.samples());
    }

    let rate =
        source_rate.ok_or_else(|| "Audiodatei enthält keine dekodierbaren Samples".to_string())?;
    let channels = source_channels.unwrap_or(0);
    if channels == 0 || samples.is_empty() {
        return Err("Audiodatei ist leer".into());
    }
    let samples = resample_stereo(&samples, rate, channels);
    if samples.is_empty() {
        return Err("Audiodatei enthält kein abspielbares Audio".into());
    }
    Ok(Arc::new(DecodedAudio { samples }))
}

fn resample_stereo(samples: &[f32], rate: u32, channels: usize) -> Vec<f32> {
    let source_frames = samples.len() / channels;
    let target_frames = source_frames.saturating_mul(SAMPLE_RATE as usize) / rate as usize;
    let mut output = Vec::with_capacity(target_frames.saturating_mul(2));
    for frame in 0..target_frames {
        let source_position = frame as f64 * rate as f64 / SAMPLE_RATE as f64;
        let first_frame = (source_position.floor() as usize).min(source_frames.saturating_sub(1));
        let second_frame = (first_frame + 1).min(source_frames.saturating_sub(1));
        let fraction = (source_position - first_frame as f64) as f32;
        let sample = |channel: usize| {
            let channel = if channels == 1 {
                0
            } else {
                channel.min(channels - 1)
            };
            let first = samples[first_frame * channels + channel];
            let second = samples[second_frame * channels + channel];
            first + (second - first) * fraction
        };
        output.push(sample(0).clamp(-1.0, 1.0));
        output.push(sample(1).clamp(-1.0, 1.0));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::resample_stereo;

    #[test]
    fn resampling_duplicates_mono_to_stereo() {
        let result = resample_stereo(&[0.25, -0.5], 48_000, 1);
        assert_eq!(result, vec![0.25, 0.25, -0.5, -0.5]);
    }

    #[test]
    fn resampling_keeps_stereo_channels_and_changes_rate() {
        let result = resample_stereo(&[0.1, -0.1, 0.2, -0.2], 24_000, 2);
        assert_eq!(result.len(), 8);
        assert_eq!(&result[..2], &[0.1, -0.1]);
        assert_eq!(&result[4..6], &[0.2, -0.2]);
    }
}
