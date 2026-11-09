use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc::Sender;

use super::CallController;

#[derive(Debug)]
pub(crate) enum CallEvent {
    Incoming {
        call_id: String,
        phone_number: String,
    },
    Outgoing {
        call_id: String,
        phone_number: String,
    },
    Active {
        call_id: String,
    },
    Ended {
        call_id: String,
    },
}

pub(crate) struct CallEventParser {
    call_index: Option<usize>,
    call_setup_index: Option<usize>,
    call: i32,
    call_setup: i32,
    active_call_id: Option<String>,
    caller_number: String,
    device_path: String,
    sender: Sender<CallEvent>,
    call_control: Arc<CallController>,
}

impl CallEventParser {
    pub(crate) fn new(
        definitions: &[String],
        current_values: &[String],
        device_path: String,
        sender: Sender<CallEvent>,
        call_control: Arc<CallController>,
    ) -> Self {
        let call_index = indicator_index(definitions, "call");
        let call_setup_index = indicator_index(definitions, "callsetup");
        let values = current_values
            .iter()
            .find_map(|line| line.strip_prefix("+CIND:"))
            .map(parse_integer_list)
            .unwrap_or_default();

        let mut parser = Self {
            call: call_index
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| values.get(index).copied())
                .unwrap_or(0),
            call_setup: call_setup_index
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| values.get(index).copied())
                .unwrap_or(0),
            call_index,
            call_setup_index,
            active_call_id: None,
            caller_number: String::new(),
            device_path,
            sender,
            call_control,
        };
        match parser.call_setup {
            1 => parser.ensure_incoming_call(),
            2 | 3 => parser.ensure_outgoing_call(),
            _ => {}
        }
        parser
    }

    pub(crate) fn process_line(&mut self, line: &str) {
        if let Some(values) = line.strip_prefix("+CIEV:") {
            let values = parse_integer_list(values);
            if let [index, value, ..] = values.as_slice() {
                if Some(*index as usize) == self.call_index {
                    self.call = *value;
                    if self.call == 1 {
                        if let Some(call_id) = &self.active_call_id {
                            self.emit(CallEvent::Active {
                                call_id: call_id.clone(),
                            });
                        }
                    }
                }
                if Some(*index as usize) == self.call_setup_index {
                    self.call_setup = *value;
                    match self.call_setup {
                        1 => self.ensure_incoming_call(),
                        2 | 3 => self.ensure_outgoing_call(),
                        _ => {}
                    }
                }
                self.finish_if_idle();
            }
            return;
        }

        if let Some(clip) = line.strip_prefix("+CLIP:") {
            if let Some(number) = first_quoted_value(clip) {
                self.caller_number = number;
                if self.active_call_id.is_none() {
                    self.ensure_incoming_call();
                }
                self.publish_incoming();
            }
            return;
        }

        if line == "RING" {
            self.ensure_incoming_call();
            return;
        }

        if line == "NO CARRIER" {
            self.call = 0;
            self.call_setup = 0;
            self.finish_if_idle();
            return;
        }

        if line == "ERROR" || line.starts_with("+CME ERROR") {
            if let Some(call_id) = self.call_control.take_pending_outgoing_call() {
                self.emit(CallEvent::Ended { call_id });
            }
        }
    }

    fn ensure_incoming_call(&mut self) {
        if self.active_call_id.is_some() {
            return;
        }

        self.active_call_id = Some(next_call_id(&self.device_path));
        self.publish_incoming();
    }

    fn ensure_outgoing_call(&mut self) {
        if self.active_call_id.is_some() {
            return;
        }

        let call_id = self
            .call_control
            .take_pending_outgoing_call()
            .unwrap_or_else(|| next_call_id(&self.device_path));
        self.active_call_id = Some(call_id.clone());
        self.emit(CallEvent::Outgoing {
            call_id,
            phone_number: self.caller_number.clone(),
        });
    }

    fn publish_incoming(&self) {
        if let Some(call_id) = &self.active_call_id {
            self.emit(CallEvent::Incoming {
                call_id: call_id.clone(),
                phone_number: self.caller_number.clone(),
            });
        }
    }

    fn finish_if_idle(&mut self) {
        if self.call == 0 && self.call_setup == 0 {
            if let Some(call_id) = self.active_call_id.take() {
                self.emit(CallEvent::Ended { call_id });
                self.caller_number.clear();
            }
        }
    }

    pub(crate) fn end_active_call(&mut self) {
        if let Some(call_id) = self.active_call_id.take() {
            self.emit(CallEvent::Ended { call_id });
        }
    }

    fn emit(&self, event: CallEvent) {
        if let Err(error) = self.sender.try_send(event) {
            eprintln!("HFP-Anrufereignis wurde verworfen: {error}");
        }
    }
}

static NEXT_CALL_ID: AtomicU64 = AtomicU64::new(1);

fn next_call_id(device_path: &str) -> String {
    let id = NEXT_CALL_ID.fetch_add(1, Ordering::Relaxed);
    format!("{device_path}#{id}")
}

fn indicator_index(definitions: &[String], name: &str) -> Option<usize> {
    let definitions = definitions.iter().find(|line| line.starts_with("+CIND:"))?;
    definitions
        .split("),")
        .enumerate()
        .find_map(|(index, definition)| {
            let quoted_name = definition.split('"').nth(1)?;
            (quoted_name == name).then_some(index + 1)
        })
}

fn parse_integer_list(input: &str) -> Vec<i32> {
    input
        .split(',')
        .filter_map(|value| value.trim().parse().ok())
        .collect()
}

fn first_quoted_value(input: &str) -> Option<String> {
    let start = input.find('"')? + 1;
    let end = input[start..].find('"')? + start;
    Some(input[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::{CallEvent, CallEventParser, first_quoted_value, indicator_index};
    use tokio::sync::mpsc;

    const INDICATORS: [&str; 1] =
        [r#"+CIND: ("service",(0,1)),("call",(0,1)),("callsetup",(0-3))"#];

    #[test]
    fn indicator_names_keep_their_one_based_hfp_indices() {
        let definitions = INDICATORS.map(str::to_string);
        assert_eq!(indicator_index(&definitions, "service"), Some(1));
        assert_eq!(indicator_index(&definitions, "call"), Some(2));
        assert_eq!(indicator_index(&definitions, "callsetup"), Some(3));
        assert_eq!(indicator_index(&definitions, "unknown"), None);
    }

    #[test]
    fn caller_number_updates_the_active_incoming_call_and_idle_ends_it() {
        let (sender, mut receiver) = mpsc::channel(8);
        let mut parser = CallEventParser::new(
            &INDICATORS.map(str::to_string),
            &["+CIND: 1,0,0".to_string()],
            "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF".to_string(),
            sender,
            Default::default(),
        );

        parser.process_line("+CIEV: 3,1");
        let Some(CallEvent::Incoming {
            call_id,
            phone_number,
        }) = receiver.try_recv().ok()
        else {
            panic!("incoming call event expected");
        };
        assert!(phone_number.is_empty());

        parser.process_line(r#"+CLIP: "+4912345",129"#);
        let Some(CallEvent::Incoming {
            call_id: updated_id,
            phone_number,
        }) = receiver.try_recv().ok()
        else {
            panic!("caller update expected");
        };
        assert_eq!(updated_id, call_id);
        assert_eq!(phone_number, "+4912345");

        parser.process_line("+CIEV: 3,0");
        assert!(
            matches!(receiver.try_recv().ok(), Some(CallEvent::Ended { call_id: ended_id }) if ended_id == call_id)
        );
    }

    #[test]
    fn quoted_values_are_optional_and_do_not_panic_on_malformed_lines() {
        assert_eq!(
            first_quoted_value(r#"+CLIP: "+123",129"#).as_deref(),
            Some("+123")
        );
        assert_eq!(first_quoted_value("+CLIP: malformed"), None);
    }
}
