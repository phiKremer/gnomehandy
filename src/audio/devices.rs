use std::{cell::RefCell, rc::Rc};

use libpulse_binding as pulse;
use pulse::{
    callbacks::ListResult,
    context::{
        Context, FlagSet,
        introspect::{CardInfo, SinkInfo, SourceInfo},
    },
    mainloop::standard::Mainloop,
    proplist::Proplist,
};
use serde::Serialize;
use zbus::{fdo, zvariant::Type};

use super::pulse::{wait_for_context, wait_for_operation};

#[derive(Clone, Copy)]
pub(crate) enum Direction {
    Input,
    Output,
}

#[derive(Serialize, Type)]
pub(crate) struct AudioDevice {
    pub name: String,
    pub description: String,
}

pub(crate) fn list_devices(direction: Direction) -> fdo::Result<Vec<AudioDevice>> {
    let pulse_direction = pulse_direction(direction);
    let is_output = matches!(direction, Direction::Output);
    let (mut mainloop, context) = open_pulse_context("GNOME Handy")?;
    let mut devices = list_card_ports(&mut mainloop, &context, pulse_direction, is_output)?;
    devices.extend(list_unbacked_devices(&mut mainloop, &context, is_output)?);
    Ok(devices)
}

fn pulse_direction(direction: Direction) -> pulse::direction::FlagSet {
    match direction {
        Direction::Input => pulse::direction::FlagSet::INPUT,
        Direction::Output => pulse::direction::FlagSet::OUTPUT,
    }
}

pub(super) fn open_pulse_context(app_name: &str) -> fdo::Result<(Mainloop, Context)> {
    let mut proplist = Proplist::new().ok_or_else(|| {
        fdo::Error::Failed("PulseAudio-Proplist konnte nicht erstellt werden".into())
    })?;
    proplist
        .set_str(pulse::proplist::properties::APPLICATION_NAME, app_name)
        .map_err(|error| {
            fdo::Error::Failed(format!(
                "PulseAudio-Proplist konnte nicht gesetzt werden: {error:?}"
            ))
        })?;
    let mut mainloop = Mainloop::new().ok_or_else(|| {
        fdo::Error::Failed("PulseAudio-Mainloop konnte nicht erstellt werden".into())
    })?;
    let mut context =
        Context::new_with_proplist(&mainloop, app_name, &proplist).ok_or_else(|| {
            fdo::Error::Failed("PulseAudio-Kontext konnte nicht erstellt werden".into())
        })?;
    context
        .connect(None, FlagSet::NOFLAGS, None)
        .map_err(|error| {
            fdo::Error::Failed(format!(
                "Verbindung zu PulseAudio/PipeWire fehlgeschlagen: {error:?}"
            ))
        })?;
    wait_for_context(&mut mainloop, &context)?;
    Ok((mainloop, context))
}

fn list_card_ports(
    mainloop: &mut Mainloop,
    context: &Context,
    direction: pulse::direction::FlagSet,
    is_output: bool,
) -> fdo::Result<Vec<AudioDevice>> {
    let devices = Rc::new(RefCell::new(Vec::new()));
    let callback_devices = Rc::clone(&devices);
    let operation =
        context
            .introspect()
            .get_card_info_list(move |result: ListResult<&CardInfo<'_>>| {
                let ListResult::Item(card) = result else {
                    return;
                };
                let card_id = card.name.as_deref().unwrap_or("unknown-card");
                let card_description = card
                    .proplist
                    .get_str("device.description")
                    .unwrap_or_else(|| card_id.to_string());
                for port in &card.ports {
                    if !port.direction.contains(direction)
                        || port.available == pulse::def::PortAvailable::No
                    {
                        continue;
                    }
                    let Some(port_id) = port.name.as_deref() else {
                        continue;
                    };
                    let port_description = port.description.as_deref().unwrap_or(port_id);
                    callback_devices.borrow_mut().push(AudioDevice {
                        name: card_port_id(card_id, port_id, is_output),
                        description: format!("{port_description} – {card_description}"),
                    });
                }
            });
    wait_for_operation(
        mainloop,
        &operation,
        "PulseAudio-Abfrage der Karten ist fehlgeschlagen",
    )?;
    Ok(std::mem::take(&mut *devices.borrow_mut()))
}

fn card_port_id(card: &str, port: &str, is_output: bool) -> String {
    if is_output {
        format!("gvc-card-port:{card}:{port}")
    } else {
        format!("gvc-card-port:input:{card}:{port}")
    }
}

fn list_unbacked_devices(
    mainloop: &mut Mainloop,
    context: &Context,
    is_output: bool,
) -> fdo::Result<Vec<AudioDevice>> {
    if is_output {
        list_unbacked_outputs(mainloop, context)
    } else {
        list_unbacked_inputs(mainloop, context)
    }
}

fn list_unbacked_outputs(
    mainloop: &mut Mainloop,
    context: &Context,
) -> fdo::Result<Vec<AudioDevice>> {
    let devices = Rc::new(RefCell::new(Vec::new()));
    let callback_devices = Rc::clone(&devices);
    let operation =
        context
            .introspect()
            .get_sink_info_list(move |result: ListResult<&SinkInfo<'_>>| {
                let ListResult::Item(sink) = result else {
                    return;
                };
                if sink.ports.is_empty() {
                    if let Some(name) = sink.name.as_ref() {
                        callback_devices.borrow_mut().push(AudioDevice {
                            name: name.to_string(),
                            description: sink.description.as_ref().unwrap_or(name).to_string(),
                        });
                    }
                }
            });
    wait_for_operation(
        mainloop,
        &operation,
        "PulseAudio-Abfrage der Ausgänge ist fehlgeschlagen",
    )?;
    Ok(std::mem::take(&mut *devices.borrow_mut()))
}

fn list_unbacked_inputs(
    mainloop: &mut Mainloop,
    context: &Context,
) -> fdo::Result<Vec<AudioDevice>> {
    let devices = Rc::new(RefCell::new(Vec::new()));
    let callback_devices = Rc::clone(&devices);
    let operation =
        context
            .introspect()
            .get_source_info_list(move |result: ListResult<&SourceInfo<'_>>| {
                let ListResult::Item(source) = result else {
                    return;
                };
                if source.ports.is_empty() {
                    if let Some(name) = source.name.as_ref() {
                        callback_devices.borrow_mut().push(AudioDevice {
                            name: name.to_string(),
                            description: source.description.as_ref().unwrap_or(name).to_string(),
                        });
                    }
                }
            });
    wait_for_operation(
        mainloop,
        &operation,
        "PulseAudio-Abfrage der Eingänge ist fehlgeschlagen",
    )?;
    Ok(std::mem::take(&mut *devices.borrow_mut()))
}
