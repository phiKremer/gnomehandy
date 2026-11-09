//! Critical workers share the lifetime of the service. A lost worker forces a
//! complete restart through systemd instead of leaving a partially working app.
use std::future::Future;

pub(crate) fn spawn_critical(name: &'static str, task: impl Future<Output = ()> + Send + 'static) {
    let worker = tokio::spawn(task);
    tokio::spawn(async move {
        let failure = worker_failure(worker).await;
        eprintln!(
            "Kritische Hintergrundaufgabe {name} beendet: {failure}; Dienst wird neu gestartet"
        );
        std::process::exit(1);
    });
}

async fn worker_failure(worker: tokio::task::JoinHandle<()>) -> String {
    match worker.await {
        Ok(()) => "unerwartetes Aufgabenende".into(),
        Err(error) => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::worker_failure;

    #[tokio::test]
    async fn detects_unexpected_worker_completion() {
        let failure = worker_failure(tokio::spawn(async {})).await;
        assert_eq!(failure, "unerwartetes Aufgabenende");
    }

    #[tokio::test]
    async fn detects_worker_panics() {
        let failure =
            worker_failure(tokio::spawn(async { panic!("injected worker failure") })).await;
        assert!(failure.contains("injected worker failure"));
    }
}
