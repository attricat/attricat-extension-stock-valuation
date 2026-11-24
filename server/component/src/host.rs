//! Thin wrappers over the `attricat:host@1.1.0` imports. The host checks
//! every call against this release's granted capabilities.

use std::collections::BTreeMap;
use std::str::FromStr;

use rust_decimal::Decimal;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::attricat::host::api::{self, ReadRequest, RecordReference, ScalarWrite, WriteRequest};

/// Entries one movement history read asks for: the host's maximum.
const MOVEMENT_PAGE: u32 = 500;
/// Records one incoming relationship read asks for: the host's maximum.
const INCOMING_PAGE: u32 = 100;
/// Writes one `api.write` call may carry.
pub const MAX_WRITE_VALUES: usize = 32;

pub fn decode<T: DeserializeOwned>(value: &str, label: &str) -> Result<T, String> {
    serde_json::from_str(value).map_err(|error| format!("Invalid {label}: {error}"))
}

pub fn configuration() -> Result<String, String> {
    api::call("configuration.get.v1", "{}")
}

/// A record as valuation needs it: its blueprint code and its active
/// values by attribute code.
pub struct RecordView {
    pub blueprint: String,
    values: BTreeMap<String, Vec<StoredValue>>,
}

struct StoredValue {
    context_id: Option<String>,
    value: Value,
    target: Option<String>,
}

#[derive(Deserialize)]
struct ReadBlueprint {
    blueprint: BlueprintIdentity,
    attributes: Vec<ReadAttribute>,
}

#[derive(Deserialize)]
struct BlueprintIdentity {
    code: String,
}

#[derive(Deserialize)]
struct ReadAttribute {
    id: String,
    code: String,
}

#[derive(Deserialize)]
struct ReadValue {
    attribute_id: String,
    value: Value,
    relationship_target_record_id: Option<String>,
    active: bool,
    context_id: Option<String>,
}

impl RecordView {
    /// The value of `code`, preferring `context_id` (the default context)
    /// when the attribute has values in several.
    fn stored(&self, code: &str, context_id: Option<&str>) -> Option<&StoredValue> {
        let values = self.values.get(code)?;
        values
            .iter()
            .find(|value| context_id.is_some() && value.context_id.as_deref() == context_id)
            .or_else(|| values.first())
    }

    pub fn number(&self, code: &str, context_id: Option<&str>) -> Option<Decimal> {
        number(&self.stored(code, context_id)?.value)
    }

    pub fn string(&self, code: &str, context_id: Option<&str>) -> Option<String> {
        self.stored(code, context_id)?
            .value
            .as_str()
            .map(str::to_owned)
    }

    pub fn target(&self, code: &str) -> Option<String> {
        self.values
            .get(code)?
            .iter()
            .find_map(|value| value.target.clone())
    }

    /// The context `code`'s value is stored in.
    pub fn context_of(&self, code: &str) -> Option<String> {
        self.values.get(code)?.first()?.context_id.clone()
    }
}

fn number(value: &Value) -> Option<Decimal> {
    match value {
        Value::Number(number) => Decimal::from_str(&number.to_string())
            .or_else(|_| Decimal::from_scientific(&number.to_string()))
            .ok(),
        Value::String(text) => Decimal::from_str(text).ok(),
        _ => None,
    }
}

pub fn read_record(record_id: &str) -> Result<RecordView, String> {
    let response = api::read(&ReadRequest::Record(RecordReference {
        record_id: record_id.into(),
    }))?;
    let blueprint: ReadBlueprint = decode(&response.blueprint, "blueprint")?;
    let codes: BTreeMap<String, String> = blueprint
        .attributes
        .into_iter()
        .map(|attribute| (attribute.id, attribute.code))
        .collect();
    let direct: Vec<ReadValue> = decode(&response.direct_values, "direct values")?;
    let mut values: BTreeMap<String, Vec<StoredValue>> = BTreeMap::new();
    for value in direct.into_iter().filter(|value| value.active) {
        let Some(code) = codes.get(&value.attribute_id) else {
            continue;
        };
        values.entry(code.clone()).or_default().push(StoredValue {
            context_id: value.context_id,
            value: value.value,
            target: value.relationship_target_record_id,
        });
    }
    Ok(RecordView {
        blueprint: blueprint.blueprint.code,
        values,
    })
}

/// One entry of a stock item's movement history.
#[derive(Clone, Debug, Deserialize)]
pub struct LedgerEntry {
    pub record_id: String,
    pub record_blueprint: String,
    /// The start of the transaction that applied it: equal for every
    /// movement one save applied.
    pub applied_at: String,
    pub balance: String,
    pub amount: String,
    pub direction: String,
}

#[derive(Deserialize)]
struct LedgerPage {
    items: Vec<LedgerEntry>,
    next_after: Option<i64>,
}

/// A record's whole movement history, in sequence order.
pub fn movement_history(record_id: &str) -> Result<Vec<LedgerEntry>, String> {
    let mut entries = Vec::new();
    let mut after = 0;
    loop {
        let request = json!({"operation": "movements", "record_id": record_id,
                             "after": after, "limit": MOVEMENT_PAGE});
        let page: LedgerPage = decode(
            &api::call("attricat.read.v1", &request.to_string())?,
            "movement history",
        )?;
        entries.extend(page.items);
        match page.next_after {
            Some(next) => after = next,
            None => return Ok(entries),
        }
    }
}

#[derive(Deserialize)]
struct IncomingPage {
    items: Vec<IncomingItem>,
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct IncomingItem {
    id: String,
}

/// The records of `blueprint` whose `field` links to `record_id`, such as
/// a production order's lines.
pub fn incoming(record_id: &str, blueprint: &str, field: &str) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let request = json!({"operation": "incoming", "record_id": record_id,
                             "relationships": [{"source_blueprint": blueprint, "field": field}],
                             "cursor": cursor, "limit": INCOMING_PAGE});
        let page: IncomingPage = decode(
            &api::call("attricat.read.v1", &request.to_string())?,
            "incoming relationships",
        )?;
        ids.extend(page.items.into_iter().map(|item| item.id));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(ids),
        }
    }
}

/// Writes numbers to one record in `context_id`. Each number is sent as
/// its exact decimal text.
pub fn write_numbers(
    record_id: &str,
    context_id: &str,
    values: &[(String, Decimal)],
) -> Result<(), String> {
    for chunk in values.chunks(MAX_WRITE_VALUES) {
        api::write(&WriteRequest {
            record_id: record_id.into(),
            values: chunk
                .iter()
                .map(|(code, value)| ScalarWrite {
                    attribute_id: None,
                    attribute_code: Some(code.clone()),
                    context_id: context_id.into(),
                    value: value.normalize().to_string(),
                })
                .collect(),
        })?;
    }
    Ok(())
}

/// Best-effort structured logging; a log failure never fails the work.
pub fn log(level: &str, message: &str) {
    let _ = api::log(level, message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_exactly() {
        assert_eq!(number(&json!(6.0625)), Some(Decimal::new(60625, 4)));
        assert_eq!(number(&json!(2425)), Some(Decimal::new(2425, 0)));
        assert_eq!(number(&json!("1.5")), Some(Decimal::new(15, 1)));
        assert_eq!(number(&json!(null)), None);
    }
}
