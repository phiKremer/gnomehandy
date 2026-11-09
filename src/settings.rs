use std::{fs, io::ErrorKind, path::PathBuf};

use zbus::fdo;

use crate::audio::{self, Direction};

pub(crate) fn get_audio_selection(file_name: &str, allow_all: bool) -> fdo::Result<String> {
    let path = audio_selection_path(file_name)?;

    match fs::read_to_string(&path) {
        Ok(name) if allow_all && !name.trim().is_empty() && name.trim().starts_with("hw:") => {
            // Previous versions stored ALSA hardware identifiers. They cannot
            // reliably be mapped to a PipeWire sink, so migrate to “all”.
            fs::write(path, "all\n").map_err(|error| {
                fdo::Error::Failed(format!(
                    "Alte Audio-Auswahl konnte nicht migriert werden: {error}"
                ))
            })?;
            Ok("all".to_string())
        }
        Ok(name) if !name.trim().is_empty() => Ok(name.trim().to_string()),
        Ok(_) if allow_all => Ok("all".to_string()),
        Ok(_) => Ok(String::new()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(if allow_all {
            "all".to_string()
        } else {
            String::new()
        }),
        Err(error) => Err(fdo::Error::Failed(format!(
            "Audio-Auswahl konnte nicht gelesen werden: {error}"
        ))),
    }
}

pub(crate) fn save_audio_selection(
    file_name: &str,
    name: &str,
    allow_all: bool,
    direction: Direction,
) -> fdo::Result<()> {
    if !(audio::list_devices(direction)?
        .iter()
        .any(|device| device.name == name)
        || allow_all && name == "all")
    {
        return Err(fdo::Error::Failed(format!(
            "Unbekanntes Audio-Gerät: {name}"
        )));
    }

    let path = audio_selection_path(file_name)?;
    let parent = path
        .parent()
        .ok_or_else(|| fdo::Error::Failed("Config-Pfad hat kein Elternverzeichnis".into()))?;
    fs::create_dir_all(parent).map_err(|error| {
        fdo::Error::Failed(format!(
            "Config-Verzeichnis konnte nicht angelegt werden: {error}"
        ))
    })?;
    fs::write(path, format!("{name}\n")).map_err(|error| {
        fdo::Error::Failed(format!(
            "Audio-Auswahl konnte nicht gespeichert werden: {error}"
        ))
    })
}

fn audio_selection_path(file_name: &str) -> fdo::Result<PathBuf> {
    let config_directory = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| fdo::Error::Failed("Kein Config-Verzeichnis gefunden".into()))?;

    Ok(config_directory.join("pkremer-gnomehandy").join(file_name))
}
