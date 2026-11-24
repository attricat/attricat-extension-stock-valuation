//! Attricat stock valuation: values stock at a moving weighted average and
//! production output at its order's material cost.
//!
//! One component on the `attricat:host@1.1.0` ABI:
//!
//! - `event-handler`: on `record.updated.v1` of a stock item (its balance
//!   changed) or of a movement record (such as a receipt whose price was
//!   entered), revalues the stock items concerned ([`valuation`]).
//! - `command-handler`: the **Recalculate valuation** record action.
//!
//! The arithmetic is host-independent (`attricat-stock-valuation-core`); the
//! configuration names the blueprints and attributes ([`config`]).

wit_bindgen::generate!({
    path: "wit",
    world: "outcome-extension",
});

mod config;
mod host;
mod valuation;

use serde::Deserialize;
use serde_json::json;

use attricat::host::api::Event;
use attricat::host::outcomes::{Failure, Rejection, Retry};
use config::Config;
use exports::attricat::host::{
    command_handler::{CommandRequest, CommandResponse},
    deferrable_operations::{BatchOutcome, OperationRequest},
    event_handler::Delivery,
};

/// The `source-name` of events this extension's own writes cause.
const OWN_SOURCE: &str = "extension:attricat-stock-valuation";
/// The event every balance change and price edit publishes.
const RECORD_UPDATED: &str = "record.updated.v1";
/// The command behind the Recalculate valuation record action.
const RECALCULATE_ACTION: &str = "recalculate-action";

/// Host call failures are temporary: the host retries the delivery with
/// backoff instead of quarantining the installation.
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Failure::Retry(Retry {
            message,
            after_ms: None,
        })
    }
}

fn rejected(code: &str, message: String, record_id: Option<String>) -> Failure {
    Failure::Rejected(Rejection {
        code: code.into(),
        message,
        record_id,
        connection: None,
    })
}

#[derive(Deserialize)]
struct RecordUpdated {
    record_id: String,
}

#[derive(Deserialize)]
struct ActionContext {
    record_ids: Vec<String>,
}

fn load_config() -> Result<Option<Config>, Failure> {
    let raw = host::configuration()?;
    Config::parse(&raw).map_err(|error| rejected("invalid_configuration", error, None))
}

/// Revalues the stock items the given records concern.
fn revalue(config: &Config, record_ids: &[String]) -> Result<valuation::Summary, String> {
    let mut session = valuation::Session::new(config);
    let mut stock_items = Vec::new();
    for record_id in record_ids {
        for stock_id in session.stock_items_of(record_id)? {
            if !stock_items.contains(&stock_id) {
                stock_items.push(stock_id);
            }
        }
    }
    if stock_items.is_empty() {
        return Ok(valuation::Summary::default());
    }
    session.run(stock_items)
}

struct Extension;

impl exports::attricat::host::event_handler::Guest for Extension {
    fn handle_event(event: Event, delivery: Delivery) -> Result<(), Failure> {
        if event.event_type != RECORD_UPDATED || delivery.source_name == OWN_SOURCE {
            return Ok(());
        }
        let Some(config) = load_config()? else {
            return Ok(());
        };
        let changed: RecordUpdated = host::decode(&event.payload, "record.updated.v1 payload")
            .map_err(|error| rejected("invalid_payload", error, None))?;
        let summary =
            revalue(&config, std::slice::from_ref(&changed.record_id)).map_err(Failure::from)?;
        for warning in &summary.warnings {
            host::log("warn", warning);
        }
        Ok(())
    }
}

impl exports::attricat::host::command_handler::Guest for Extension {
    fn handle_command(request: CommandRequest) -> Result<CommandResponse, String> {
        if request.handler != RECALCULATE_ACTION {
            return Err(format!("Unknown command handler `{}`.", request.handler));
        }
        let context: ActionContext = host::decode(&request.payload, "action context")?;
        let notification = match load_config() {
            Err(_) => {
                json!({"message": "The valuation configuration is invalid.", "severity": "error"})
            }
            Ok(None) => {
                json!({"message": "Stock valuation is not configured.", "severity": "warning"})
            }
            Ok(Some(config)) => {
                let summary = revalue(&config, &context.record_ids)?;
                let message = if summary.warnings.is_empty() {
                    format!(
                        "Revalued {} stock items; {} records changed.",
                        summary.stock_items, summary.records_written
                    )
                } else {
                    format!(
                        "Revalued {} stock items; {} records changed. {}",
                        summary.stock_items,
                        summary.records_written,
                        summary.warnings.join(" ")
                    )
                };
                let severity = if summary.warnings.is_empty() {
                    "success"
                } else {
                    "warning"
                };
                json!({"message": message, "severity": severity})
            }
        };
        Ok(CommandResponse {
            payload: json!({"notification": notification, "refresh": true}).to_string(),
        })
    }
}

/// The world requires operations; this release declares none.
impl exports::attricat::host::deferrable_operations::Guest for Extension {
    fn prepare(_request: OperationRequest) -> Result<String, String> {
        Err("This extension has no operations.".into())
    }

    fn start(_request: OperationRequest) -> Result<String, String> {
        Err("This extension has no operations.".into())
    }

    fn process_batch(_request: OperationRequest) -> Result<BatchOutcome, Failure> {
        Err(rejected(
            "no_operations",
            "This extension has no operations.".into(),
            None,
        ))
    }

    fn checkpoint(_request: OperationRequest) -> Result<(), String> {
        Ok(())
    }

    fn finish(_request: OperationRequest) -> Result<(), String> {
        Ok(())
    }

    fn cancel(_request: OperationRequest) -> Result<(), String> {
        Ok(())
    }
}

export!(Extension);
