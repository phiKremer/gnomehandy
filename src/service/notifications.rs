use std::collections::HashMap;

use zbus::{Connection, zvariant::OwnedValue};

pub(crate) async fn publish_sms_received(
    connection: &Connection,
    sender: &str,
    body: &str,
) -> zbus::Result<()> {
    let summary = if sender.is_empty() {
        "Neue SMS".to_string()
    } else {
        format!("SMS von {sender}")
    };
    let actions: Vec<&str> = Vec::new();
    let mut hints = HashMap::<String, OwnedValue>::new();
    hints.insert(
        "sound-name".into(),
        OwnedValue::try_from(zbus::zvariant::Value::from("message-new-instant"))?,
    );

    connection
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                "GNOME Handy",
                0_u32,
                "mail-message-new-symbolic",
                summary,
                body,
                actions,
                hints,
                10_000_i32,
            ),
        )
        .await?;
    Ok(())
}
