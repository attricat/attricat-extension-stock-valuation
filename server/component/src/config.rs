//! The installation configuration: which blueprints and attributes play
//! which part in valuation. Nothing is tied to one solution pack; a pack
//! that installs the extension supplies these codes.

use serde::Deserialize;

/// Currency minor units unless configured otherwise.
const DEFAULT_DECIMAL_PLACES: u32 = 2;
/// The most decimal places a value is rounded to.
pub const MAX_DECIMAL_PLACES: u32 = 6;

fn decimal_places() -> u32 {
    DEFAULT_DECIMAL_PLACES
}
fn quantity() -> String {
    "quantity".into()
}
fn value() -> String {
    "value".into()
}
fn unit_cost() -> String {
    "unit_cost".into()
}
fn average_cost() -> String {
    "average_cost".into()
}
fn stock_item() -> String {
    "stock_item".into()
}
fn reverses() -> Option<String> {
    Some("reverses".into())
}
fn unit_price() -> String {
    "unit_price_net".into()
}
fn production_order() -> String {
    "production_order".into()
}
fn kind() -> String {
    "kind".into()
}
fn item() -> String {
    "item".into()
}
fn base_quantity() -> String {
    "base_quantity".into()
}
fn material_cost() -> String {
    "material_cost".into()
}
fn production_cost() -> String {
    "production_cost".into()
}
fn scrap_price() -> String {
    "scrap_price".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Decimal places values are rounded to: the currency's minor unit.
    #[serde(default = "decimal_places")]
    pub decimal_places: u32,
    pub stock_item: StockItem,
    #[serde(default)]
    pub receipts: Vec<Receipt>,
    #[serde(default)]
    pub issues: Vec<Issue>,
    pub production: Option<Production>,
}

/// The records holding stock: their quantity balance and where the stock's
/// value goes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StockItem {
    pub blueprint: String,
    /// The balance movements change; other balances, such as a reservation,
    /// carry no value.
    #[serde(default = "quantity")]
    pub quantity: String,
    #[serde(default = "value")]
    pub value: String,
    #[serde(default = "average_cost")]
    pub average_cost: String,
}

/// Records that bring stock in at their own price, such as goods receipts.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub blueprint: String,
    /// The quantity in the unit the price is for, such as the document's.
    #[serde(default = "quantity")]
    pub quantity: String,
    /// Net price per unit of `quantity`.
    #[serde(default = "unit_price")]
    pub unit_price: String,
    /// Multiplies the price into the stock's currency; absent means 1.
    pub exchange_rate: Option<String>,
    #[serde(default = "reverses")]
    pub reverses: Option<String>,
    #[serde(default = "stock_item")]
    pub stock_item: String,
    #[serde(default = "unit_cost")]
    pub unit_cost: String,
    #[serde(default = "value")]
    pub value: String,
}

/// Records that take stock out at its average cost, such as goods issues.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issue {
    pub blueprint: String,
    #[serde(default = "reverses")]
    pub reverses: Option<String>,
    #[serde(default = "stock_item")]
    pub stock_item: String,
    #[serde(default = "unit_cost")]
    pub unit_cost: String,
    #[serde(default = "value")]
    pub value: String,
}

/// Production orders: material lines consumed at average cost and output
/// lines that share the material cost.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Production {
    pub order: Order,
    pub material: MaterialLine,
    pub output: OutputLine,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Order {
    pub blueprint: String,
    #[serde(default = "material_cost")]
    pub material_cost: String,
    #[serde(default = "production_cost")]
    pub production_cost: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialLine {
    pub blueprint: String,
    #[serde(default = "production_order")]
    pub order: String,
    #[serde(default = "stock_item")]
    pub stock_item: String,
    #[serde(default = "item")]
    pub item: String,
    #[serde(default = "base_quantity")]
    pub base_quantity: String,
    #[serde(default = "unit_cost")]
    pub unit_cost: String,
    #[serde(default = "value")]
    pub value: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputLine {
    pub blueprint: String,
    #[serde(default = "production_order")]
    pub order: String,
    #[serde(default = "stock_item")]
    pub stock_item: String,
    /// `product`, `remnant` or `scrap`.
    #[serde(default = "kind")]
    pub kind: String,
    #[serde(default = "item")]
    pub item: String,
    #[serde(default = "base_quantity")]
    pub base_quantity: String,
    /// Scrap's buy-back price per base unit, on the output line's item.
    #[serde(default = "scrap_price")]
    pub scrap_price: String,
    #[serde(default = "unit_cost")]
    pub unit_cost: String,
    #[serde(default = "value")]
    pub value: String,
}

/// What a movement record is to valuation.
pub enum Role<'a> {
    Receipt(&'a Receipt),
    Issue(&'a Issue),
    Material(&'a MaterialLine),
    Output(&'a OutputLine),
}

impl Config {
    /// The configuration, or `None` while the extension is not configured.
    pub fn parse(raw: &str) -> Result<Option<Self>, String> {
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|error| format!("Invalid configuration: {error}"))?;
        if value.as_object().is_none_or(|object| object.is_empty()) {
            return Ok(None);
        }
        let config: Self = serde_json::from_value(value)
            .map_err(|error| format!("Invalid configuration: {error}"))?;
        if config.decimal_places > MAX_DECIMAL_PLACES {
            return Err(format!(
                "Invalid configuration: decimal_places must be 0-{MAX_DECIMAL_PLACES}"
            ));
        }
        Ok(Some(config))
    }

    pub fn role(&self, blueprint: &str) -> Option<Role<'_>> {
        if let Some(receipt) = self.receipts.iter().find(|r| r.blueprint == blueprint) {
            return Some(Role::Receipt(receipt));
        }
        if let Some(issue) = self.issues.iter().find(|i| i.blueprint == blueprint) {
            return Some(Role::Issue(issue));
        }
        let production = self.production.as_ref()?;
        if production.material.blueprint == blueprint {
            return Some(Role::Material(&production.material));
        }
        if production.output.blueprint == blueprint {
            return Some(Role::Output(&production.output));
        }
        None
    }

    /// The relationship a movement record links its stock item through.
    pub fn stock_item_link(&self, blueprint: &str) -> Option<&str> {
        Some(match self.role(blueprint)? {
            Role::Receipt(receipt) => &receipt.stock_item,
            Role::Issue(issue) => &issue.stock_item,
            Role::Material(line) => &line.stock_item,
            Role::Output(line) => &line.stock_item,
        })
    }

    /// Where a movement record's own cost goes.
    pub fn cost_attributes(&self, blueprint: &str) -> Option<(&str, &str)> {
        Some(match self.role(blueprint)? {
            Role::Receipt(receipt) => (&receipt.unit_cost, &receipt.value),
            Role::Issue(issue) => (&issue.unit_cost, &issue.value),
            Role::Material(line) => (&line.unit_cost, &line.value),
            Role::Output(line) => (&line.unit_cost, &line.value),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_configuration_turns_valuation_off() {
        assert!(Config::parse("{}").unwrap().is_none());
        assert!(Config::parse("null").unwrap().is_none());
    }

    #[test]
    fn attribute_codes_default_to_the_inventory_pack() {
        let config = Config::parse(
            r#"{"stock_item": {"blueprint": "inv_stock_item"},
                "receipts": [{"blueprint": "inv_goods_receipt"}]}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.decimal_places, 2);
        assert_eq!(config.stock_item.quantity, "quantity");
        let Some(Role::Receipt(receipt)) = config.role("inv_goods_receipt") else {
            panic!("a receipt");
        };
        assert_eq!(receipt.unit_price, "unit_price_net");
        assert_eq!(receipt.reverses.as_deref(), Some("reverses"));
        assert!(config.role("inv_goods_issue").is_none());
    }

    #[test]
    fn unknown_fields_and_wide_rounding_are_refused() {
        assert!(Config::parse(r#"{"stock_item": {"blueprint": "s", "typo": 1}}"#).is_err());
        assert!(
            Config::parse(r#"{"stock_item": {"blueprint": "s"}, "decimal_places": 9}"#).is_err()
        );
    }
}
