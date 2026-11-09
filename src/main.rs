mod app;
mod audio;
mod bluetooth;
mod call_events;
mod cli;
mod ringtone;
mod runtime;
mod service;
mod settings;
mod sms;

use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    app::run().await
}
