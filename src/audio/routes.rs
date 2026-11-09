use std::{cell::RefCell, rc::Rc};

use libpulse_binding as pulse;
use pulse::{
    callbacks::ListResult,
    context::{Context, FlagSet},
    mainloop::standard::Mainloop,
};
use zbus::fdo;

use super::devices::open_pulse_context;
use super::pulse::{wait_for_context, wait_for_operation};
use super::{AudioDevice, Direction, list_devices};

#[derive(Clone, Debug)]
pub(crate) struct AudioRoute {
    pub sink_name: String,
    pub sink_index: u32,
    pub port_name: Option<String>,
    pub active_port: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct InputRoute {
    pub source_name: String,
    pub source_index: u32,
    pub port_name: Option<String>,
    pub active_port: Option<String>,
}

pub(crate) fn output_routes(selection: &str) -> fdo::Result<Vec<AudioRoute>> {
    let devices = selected_output_devices(selection)?;
    let (mut mainloop, context) = open_pulse_context("GNOME Handy Klingelton")?;
    let card_indices = list_card_indices(&mut mainloop, &context)?;
    let sinks = list_sink_routes(&mut mainloop, &context)?;
    let routes = resolve_output_routes(&devices, &card_indices, &sinks);
    if routes.is_empty() {
        return Err(fdo::Error::Failed(
            "Für die Klingelton-Auswahl wurden keine erreichbaren Audioausgänge gefunden".into(),
        ));
    }
    Ok(routes)
}

fn selected_output_devices(selection: &str) -> fdo::Result<Vec<AudioDevice>> {
    let devices = list_devices(Direction::Output)?;
    if selection == "all" {
        return Ok(devices);
    }
    if devices.iter().any(|device| device.name == selection) {
        return Ok(vec![AudioDevice {
            name: selection.to_string(),
            description: String::new(),
        }]);
    }
    Err(fdo::Error::Failed(format!(
        "Unbekannter Klingelton-Ausgang: {selection}"
    )))
}

fn list_card_indices(
    mainloop: &mut Mainloop,
    context: &Context,
) -> fdo::Result<std::collections::HashMap<String, u32>> {
    let cards = Rc::new(RefCell::new(std::collections::HashMap::new()));
    let callback_cards = Rc::clone(&cards);
    let operation = context.introspect().get_card_info_list(move |result| {
        if let ListResult::Item(card) = result {
            if let Some(name) = card.name.as_deref() {
                callback_cards
                    .borrow_mut()
                    .insert(name.to_string(), card.index);
            }
        }
    });
    wait_for_operation(
        mainloop,
        &operation,
        "PulseAudio-Abfrage der Karten ist fehlgeschlagen",
    )?;
    Ok(std::mem::take(&mut *cards.borrow_mut()))
}

type SinkRecord = (Option<u32>, AudioRoute, Vec<String>);

fn list_sink_routes(mainloop: &mut Mainloop, context: &Context) -> fdo::Result<Vec<SinkRecord>> {
    let sinks = Rc::new(RefCell::new(Vec::new()));
    let callback_sinks = Rc::clone(&sinks);
    let operation = context.introspect().get_sink_info_list(move |result| {
        let ListResult::Item(sink) = result else {
            return;
        };
        let Some(name) = sink.name.as_deref() else {
            return;
        };
        callback_sinks.borrow_mut().push((
            sink.card,
            AudioRoute {
                sink_name: name.to_string(),
                sink_index: sink.index,
                port_name: None,
                active_port: sink
                    .active_port
                    .as_ref()
                    .and_then(|port| port.name.as_deref())
                    .map(str::to_string),
            },
            sink.ports
                .iter()
                .filter_map(|port| port.name.as_deref().map(str::to_string))
                .collect(),
        ));
    });
    wait_for_operation(
        mainloop,
        &operation,
        "PulseAudio-Abfrage der Ausgänge ist fehlgeschlagen",
    )?;
    Ok(std::mem::take(&mut *sinks.borrow_mut()))
}

fn resolve_output_routes(
    devices: &[AudioDevice],
    card_indices: &std::collections::HashMap<String, u32>,
    sinks: &[SinkRecord],
) -> Vec<AudioRoute> {
    let mut routes = Vec::new();
    for device in devices {
        if let Some(route) = resolve_card_port_route(&device.name, card_indices, sinks) {
            routes.extend(route);
        } else if let Some((_, route, _)) = sinks
            .iter()
            .find(|(_, route, _)| route.sink_name == device.name)
        {
            routes.push(route.clone());
        }
    }
    routes
}

fn resolve_card_port_route(
    device_name: &str,
    card_indices: &std::collections::HashMap<String, u32>,
    sinks: &[SinkRecord],
) -> Option<Vec<AudioRoute>> {
    let rest = device_name.strip_prefix("gvc-card-port:")?;
    let (card_name, port_name) = rest.split_once(':')?;
    let card_index = card_indices.get(card_name)?;
    // A card-backed sink can expose multiple ports; retain the requested port on every match.
    Some(
        sinks
            .iter()
            .filter(|(card, _, ports)| {
                *card == Some(*card_index) && ports.iter().any(|port| port == port_name)
            })
            .map(|(_, route, _)| {
                let mut route = route.clone();
                route.port_name = Some(port_name.to_string());
                route
            })
            .collect(),
    )
}

pub(crate) fn input_route(selection: &str) -> fdo::Result<InputRoute> {
    if !list_devices(Direction::Input)?
        .iter()
        .any(|device| device.name == selection)
    {
        return Err(fdo::Error::Failed(format!(
            "Unbekanntes Mikrofon: {selection}"
        )));
    }
    let (mut mainloop, context) = open_pulse_context("GNOME Handy Gesprächseingang")?;
    let (card_name, port_name) = parse_input_port(selection);
    let card_index = match card_name.as_deref() {
        Some(card_name) => list_card_indices(&mut mainloop, &context)?
            .get(card_name)
            .copied(),
        None => None,
    };
    let selected = find_input_route(&mut mainloop, &context, selection, card_index, port_name)?;
    selected.ok_or_else(|| {
        fdo::Error::Failed(format!(
            "PulseAudio-Mikrofon für Auswahl {selection} wurde nicht gefunden"
        ))
    })
}

fn parse_input_port(selection: &str) -> (Option<String>, Option<String>) {
    selection
        .strip_prefix("gvc-card-port:input:")
        .and_then(|rest| rest.split_once(':'))
        .map(|(card, port)| (Some(card.to_string()), Some(port.to_string())))
        .unwrap_or((None, None))
}

fn find_input_route(
    mainloop: &mut Mainloop,
    context: &Context,
    selection: &str,
    card_index: Option<u32>,
    port_name: Option<String>,
) -> fdo::Result<Option<InputRoute>> {
    let selected = Rc::new(RefCell::new(None));
    let callback_selected = Rc::clone(&selected);
    let selection = selection.to_string();
    let operation = context.introspect().get_source_info_list(move |result| {
        let ListResult::Item(source) = result else {
            return;
        };
        let Some(name) = source.name.as_deref() else {
            return;
        };
        let is_match = match card_index {
            Some(card_index) => {
                source.card == Some(card_index)
                    && port_name.as_ref().is_some_and(|wanted| {
                        source
                            .ports
                            .iter()
                            .any(|port| port.name.as_deref() == Some(wanted.as_str()))
                    })
            }
            None => name == selection,
        };
        if is_match {
            callback_selected.replace(Some(InputRoute {
                source_name: name.to_string(),
                source_index: source.index,
                port_name: port_name.clone(),
                active_port: source
                    .active_port
                    .as_ref()
                    .and_then(|port| port.name.as_deref())
                    .map(str::to_string),
            }));
        }
    });
    wait_for_operation(
        mainloop,
        &operation,
        "PulseAudio-Abfrage der Mikrofone ist fehlgeschlagen",
    )?;
    Ok(selected.borrow_mut().take())
}

pub(crate) fn set_output_port(sink_index: u32, port: &str) -> Result<(), String> {
    set_device_port(sink_index, port, true)
}

pub(crate) fn set_input_port(source_index: u32, port: &str) -> Result<(), String> {
    set_device_port(source_index, port, false)
}

fn set_device_port(index: u32, port: &str, is_output: bool) -> Result<(), String> {
    let mut mainloop = Mainloop::new().ok_or("PulseAudio-Mainloop konnte nicht erstellt werden")?;
    let mut context = Context::new(&mainloop, "GNOME Handy Gesprächsgerät")
        .ok_or("PulseAudio-Kontext konnte nicht erstellt werden")?;
    context
        .connect(None, FlagSet::NOFLAGS, None)
        .map_err(|error| format!("PulseAudio-Verbindung fehlgeschlagen: {error:?}"))?;
    wait_for_context(&mut mainloop, &context).map_err(|error| error.to_string())?;
    let completed = Rc::new(RefCell::new(None));
    let result = Rc::clone(&completed);
    let operation = if is_output {
        context.introspect().set_sink_port_by_index(
            index,
            port,
            Some(Box::new(move |success| {
                *result.borrow_mut() = Some(success);
            })),
        )
    } else {
        context.introspect().set_source_port_by_index(
            index,
            port,
            Some(Box::new(move |success| {
                *result.borrow_mut() = Some(success);
            })),
        )
    };
    wait_for_operation(
        &mut mainloop,
        &operation,
        "PulseAudio-Portwechsel ist fehlgeschlagen",
    )
    .map_err(|error| error.to_string())?;
    if *completed.borrow() == Some(true) {
        Ok(())
    } else {
        Err(format!("PulseAudio lehnte den Port {port} ab"))
    }
}
