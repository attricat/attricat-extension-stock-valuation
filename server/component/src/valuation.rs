//! One valuation pass: replays stock items' movement histories, values
//! production output from its order's consumed materials, and writes the
//! results that changed.
//!
//! A pass starts from the stock items an event or a command names and
//! follows what depends on them: a material's cost feeds every order that
//! consumed it, and that order's output feeds its products' stock items.
//! Every history is replayed from the start, so a pass gives the same result
//! however events were ordered or repeated.
//!
//! Remnants and scrap usually go back into the stock item the order consumed
//! from. They are valued without the order's full allocation: scrap at its
//! item's scrap price, a remnant at what the order's consumption of the same
//! item cost, read from that stock item's replay so far.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::str::FromStr;

use attricat_stock_valuation_core::{
    self as core, Allocation, Cost, Direction, Movement, Output, OutputKind, Replay, Replayer,
};
use rust_decimal::Decimal;

use crate::config::{Config, Production, Role};
use crate::host::{self, LedgerEntry, RecordView};

/// Stock items one pass revalues at most, so a long chain of orders cannot
/// exhaust the component's fuel.
const MAX_STOCK_ITEMS_PER_PASS: usize = 200;

/// What a pass did.
#[derive(Default, Debug)]
pub struct Summary {
    pub stock_items: usize,
    pub records_written: usize,
    pub warnings: Vec<String>,
}

pub struct Session<'a> {
    config: &'a Config,
    records: HashMap<String, RecordView>,
    replays: HashMap<String, StockReplay>,
    /// Stock items being replayed, with what they have replayed so far.
    partials: HashMap<String, Replayer>,
    allocations: HashMap<String, Allocation>,
    allocating: HashSet<String>,
    /// Desired numbers per record, written at the end when they changed.
    results: BTreeMap<String, BTreeMap<String, Decimal>>,
    /// The default context, where balances and their documents' values live.
    context_id: Option<String>,
    summary: Summary,
}

struct StockReplay {
    replay: Replay,
    entries: Vec<LedgerEntry>,
}

/// What one consumed material line cost.
struct Consumed {
    item: Option<String>,
    value: Decimal,
    amount: Decimal,
}

impl<'a> Session<'a> {
    pub fn new(config: &'a Config) -> Self {
        Self {
            config,
            records: HashMap::new(),
            replays: HashMap::new(),
            partials: HashMap::new(),
            allocations: HashMap::new(),
            allocating: HashSet::new(),
            results: BTreeMap::new(),
            context_id: None,
            summary: Summary::default(),
        }
    }

    fn record(&mut self, record_id: &str) -> Result<&RecordView, String> {
        if !self.records.contains_key(record_id) {
            let record = host::read_record(record_id)?;
            self.records.insert(record_id.to_owned(), record);
        }
        Ok(&self.records[record_id])
    }

    fn production(&self) -> Result<&'a Production, String> {
        self.config
            .production
            .as_ref()
            .ok_or_else(|| "Production is not configured.".to_owned())
    }

    /// The stock items to revalue for a changed record: itself when it is a
    /// stock item, the one it moves when it is a movement record, and the
    /// ones an order's lines move.
    pub fn stock_items_of(&mut self, record_id: &str) -> Result<Vec<String>, String> {
        let config = self.config;
        let record = self.record(record_id)?;
        if record.blueprint == config.stock_item.blueprint {
            return Ok(vec![record_id.to_owned()]);
        }
        if let Some(production) = &config.production
            && record.blueprint == production.order.blueprint
        {
            let mut stock_items = BTreeSet::new();
            for (blueprint, order, stock_item) in [
                (
                    &production.material.blueprint,
                    &production.material.order,
                    &production.material.stock_item,
                ),
                (
                    &production.output.blueprint,
                    &production.output.order,
                    &production.output.stock_item,
                ),
            ] {
                for line_id in host::incoming(record_id, blueprint, order)? {
                    stock_items.extend(self.record(&line_id)?.target(stock_item));
                }
            }
            return Ok(stock_items.into_iter().collect());
        }
        let Some(link) = config.stock_item_link(&record.blueprint) else {
            return Ok(Vec::new());
        };
        Ok(record.target(link).into_iter().collect())
    }

    /// Revalues `stock_items` and everything that depends on them, then
    /// writes what changed.
    pub fn run(mut self, stock_items: Vec<String>) -> Result<Summary, String> {
        let mut queue: VecDeque<String> = stock_items.into();
        let mut queued: HashSet<String> = queue.iter().cloned().collect();
        let mut revalued = 0;
        while let Some(stock_id) = queue.pop_front() {
            revalued += 1;
            if revalued > MAX_STOCK_ITEMS_PER_PASS {
                self.summary.warnings.push(format!(
                    "Stopped after {MAX_STOCK_ITEMS_PER_PASS} stock items; recalculate the rest separately."
                ));
                break;
            }
            self.replay_stock(&stock_id)?;
            for dependent in self.dependents(&stock_id)? {
                if queued.insert(dependent.clone()) {
                    queue.push_back(dependent);
                }
            }
        }
        self.summary.stock_items = self.replays.len();
        self.write()?;
        Ok(self.summary)
    }

    /// Replays one stock item's quantity history once per pass.
    fn replay_stock(&mut self, stock_id: &str) -> Result<(), String> {
        if self.replays.contains_key(stock_id) {
            return Ok(());
        }
        if self.partials.contains_key(stock_id) {
            return Err(format!(
                "Stock item {stock_id} is valued from its own finished products."
            ));
        }
        let config = self.config;
        let context_id = self
            .record(stock_id)?
            .context_of(&config.stock_item.quantity);
        if self.context_id.is_none() {
            self.context_id = context_id;
        }
        let entries = consumption_first(
            host::movement_history(stock_id)?
                .into_iter()
                .filter(|entry| entry.balance == config.stock_item.quantity)
                .collect(),
            config
                .production
                .as_ref()
                .map(|production| production.output.blueprint.as_str()),
        );
        self.partials
            .insert(stock_id.to_owned(), Replayer::new(config.decimal_places));
        for entry in &entries {
            let amount = Decimal::from_str(&entry.amount)
                .map_err(|_| format!("Invalid movement amount `{}`.", entry.amount))?;
            let direction = if entry.direction == "in" {
                Direction::In
            } else {
                Direction::Out
            };
            let cost = self.cost(entry, direction, amount)?;
            let movement = Movement {
                record_id: entry.record_id.clone(),
                amount,
                direction,
                cost,
            };
            if let Some(replayer) = self.partials.get_mut(stock_id) {
                replayer.step(&movement);
            }
        }
        let replay = self
            .partials
            .remove(stock_id)
            .map(Replayer::finish)
            .unwrap_or_default();
        for entry in &entries {
            if let (Some((unit_cost, value)), Some(valued)) = (
                config.cost_attributes(&entry.record_blueprint),
                replay.movements.get(&entry.record_id),
            ) {
                self.result(&entry.record_id, unit_cost, valued.unit_cost);
                self.result(&entry.record_id, value, valued.value);
            }
        }
        self.result(stock_id, &config.stock_item.value, replay.value);
        self.result(
            stock_id,
            &config.stock_item.average_cost,
            replay.average_cost(),
        );
        self.replays
            .insert(stock_id.to_owned(), StockReplay { replay, entries });
        Ok(())
    }

    /// How one movement of a stock item's history is valued.
    fn cost(
        &mut self,
        entry: &LedgerEntry,
        direction: Direction,
        amount: Decimal,
    ) -> Result<Cost, String> {
        let config = self.config;
        let context = self.context_id.clone();
        let context = context.as_deref();
        Ok(match (config.role(&entry.record_blueprint), direction) {
            (Some(Role::Receipt(receipt)), Direction::In) => {
                let record = self.record(&entry.record_id)?;
                if let Some(original) = reversed(record, receipt.reverses.as_deref()) {
                    Cost::Same(original)
                } else {
                    let quantity = record.number(&receipt.quantity, context);
                    let price = record.number(&receipt.unit_price, context);
                    let rate = match &receipt.exchange_rate {
                        Some(code) => record.number(code, context).unwrap_or(Decimal::ONE),
                        None => Decimal::ONE,
                    };
                    match (quantity, price) {
                        (Some(quantity), Some(price)) => Cost::Value(quantity * price * rate),
                        _ => {
                            self.summary.warnings.push(format!(
                                "Receipt {} has no price; it is valued at zero.",
                                entry.record_id
                            ));
                            Cost::Value(Decimal::ZERO)
                        }
                    }
                }
            }
            (Some(Role::Receipt(receipt)), Direction::Out) => {
                let record = self.record(&entry.record_id)?;
                reversed(record, receipt.reverses.as_deref()).map_or(Cost::Average, Cost::Same)
            }
            (Some(Role::Issue(issue)), Direction::In) => {
                let record = self.record(&entry.record_id)?;
                reversed(record, issue.reverses.as_deref()).map_or(Cost::Average, Cost::Same)
            }
            (Some(Role::Output(_)), Direction::In) => self.output_cost(&entry.record_id, amount)?,
            // Issues and consumption leave at the average, as does anything
            // the configuration does not name.
            _ => Cost::Average,
        })
    }

    /// An output line's cost: scrap and remnants by their own rule, finished
    /// products by their order's allocation.
    fn output_cost(&mut self, line_id: &str, amount: Decimal) -> Result<Cost, String> {
        let production = self.production()?;
        let context = self.context_id.clone();
        let context = context.as_deref();
        let line = self.record(line_id)?;
        let kind = line
            .string(&production.output.kind, context)
            .as_deref()
            .and_then(OutputKind::parse)
            .unwrap_or(OutputKind::Product);
        let item = line.target(&production.output.item);
        let order = line
            .target(&production.output.order)
            .ok_or_else(|| format!("Output line {line_id} has no production order."))?;
        Ok(match kind {
            OutputKind::Scrap => Cost::Value(amount * self.scrap_price(item.as_deref())?),
            OutputKind::Remnant => {
                Cost::Value(amount * self.remnant_unit_cost(&order, item.as_deref())?)
            }
            OutputKind::Product => self
                .allocation(&order)?
                .outputs
                .get(line_id)
                .map_or(Cost::Average, |valued| Cost::Value(valued.value)),
        })
    }

    fn scrap_price(&mut self, item: Option<&str>) -> Result<Decimal, String> {
        let production = self.production()?;
        let context = self.context_id.clone();
        Ok(match item {
            Some(item) => self
                .record(item)?
                .number(&production.output.scrap_price, context.as_deref())
                .unwrap_or_default(),
            None => Decimal::ZERO,
        })
    }

    /// What the order's consumption of `item` cost per base unit, or of all
    /// its materials when it consumed none of that item. Exact; the caller
    /// rounds the value.
    fn remnant_unit_cost(&mut self, order: &str, item: Option<&str>) -> Result<Decimal, String> {
        let consumed = self.consumed(order)?;
        let total = |lines: &mut dyn Iterator<Item = &Consumed>| {
            lines.fold((Decimal::ZERO, Decimal::ZERO), |total, line| {
                (total.0 + line.value, total.1 + line.amount)
            })
        };
        let same = total(
            &mut consumed
                .iter()
                .filter(|line| item.is_some() && line.item.as_deref() == item),
        );
        let (value, amount) = if same.1.is_zero() {
            total(&mut consumed.iter())
        } else {
            same
        };
        Ok(if amount.is_zero() {
            Decimal::ZERO
        } else {
            value / amount
        })
    }

    /// What each of the order's material lines cost when it was consumed.
    fn consumed(&mut self, order: &str) -> Result<Vec<Consumed>, String> {
        let production = self.production()?;
        let scale = self.config.decimal_places;
        let context = self.context_id.clone();
        let context = context.as_deref();
        let mut consumed = Vec::new();
        for line_id in host::incoming(
            order,
            &production.material.blueprint,
            &production.material.order,
        )? {
            let line = self.record(&line_id)?;
            let (Some(stock_id), Some(amount)) = (
                line.target(&production.material.stock_item),
                line.number(&production.material.base_quantity, context),
            ) else {
                continue;
            };
            let item = line.target(&production.material.item);
            let value = if let Some(partial) = self.partials.get(&stock_id) {
                // That stock item is being replayed: its consumption is
                // either already replayed or, applied in the same save, next
                // at the average.
                let state = partial.state();
                state.movements.get(&line_id).map_or_else(
                    || core::round(amount * state.value / nonzero(state.quantity), scale),
                    |valued| valued.value,
                )
            } else {
                self.replay_stock(&stock_id)?;
                match self.replays[&stock_id].replay.movements.get(&line_id) {
                    Some(valued) => valued.value,
                    // Not consumed: the order is not completed.
                    None => continue,
                }
            };
            consumed.push(Consumed {
                item,
                value,
                amount,
            });
        }
        Ok(consumed)
    }

    /// The order's material cost split over its output, once per pass.
    fn allocation(&mut self, order: &str) -> Result<Allocation, String> {
        if let Some(allocation) = self.allocations.get(order) {
            return Ok(allocation.clone());
        }
        if !self.allocating.insert(order.to_owned()) {
            return Err(format!(
                "Production order {order} consumes its own finished products."
            ));
        }
        let production = self.production()?;
        let context = self.context_id.clone();
        let context = context.as_deref();
        let material_cost: Decimal = self.consumed(order)?.iter().map(|line| line.value).sum();
        let mut outputs = Vec::new();
        for line_id in host::incoming(
            order,
            &production.output.blueprint,
            &production.output.order,
        )? {
            let line = self.record(&line_id)?;
            let kind = line
                .string(&production.output.kind, context)
                .as_deref()
                .and_then(OutputKind::parse)
                .unwrap_or(OutputKind::Product);
            let amount = line
                .number(&production.output.base_quantity, context)
                .unwrap_or_default();
            let item = line.target(&production.output.item);
            let unit_price = match kind {
                OutputKind::Product => Decimal::ZERO,
                OutputKind::Remnant => self.remnant_unit_cost(order, item.as_deref())?,
                OutputKind::Scrap => self.scrap_price(item.as_deref())?,
            };
            outputs.push(Output {
                record_id: line_id,
                kind,
                amount,
                unit_price,
            });
        }
        let allocation = core::allocate(material_cost, &outputs, self.config.decimal_places);
        self.result(
            order,
            &production.order.material_cost,
            allocation.material_cost,
        );
        self.result(
            order,
            &production.order.production_cost,
            allocation.production_cost,
        );
        self.allocating.remove(order);
        self.allocations
            .insert(order.to_owned(), allocation.clone());
        Ok(allocation)
    }

    /// The stock items whose value depends on `stock_id`'s: those holding
    /// the output of every order that consumed from it.
    fn dependents(&mut self, stock_id: &str) -> Result<Vec<String>, String> {
        let Some(production) = self.config.production.as_ref() else {
            return Ok(Vec::new());
        };
        let consumed: Vec<String> = self.replays[stock_id]
            .entries
            .iter()
            .filter(|entry| entry.record_blueprint == production.material.blueprint)
            .map(|entry| entry.record_id.clone())
            .collect();
        let mut orders = BTreeSet::new();
        for line_id in consumed {
            if let Some(order) = self.record(&line_id)?.target(&production.material.order) {
                orders.insert(order);
            }
        }
        let mut dependents = BTreeSet::new();
        for order in orders {
            for line_id in host::incoming(
                &order,
                &production.output.blueprint,
                &production.output.order,
            )? {
                if let Some(stock) = self.record(&line_id)?.target(&production.output.stock_item)
                    && stock != stock_id
                {
                    dependents.insert(stock);
                }
            }
        }
        Ok(dependents.into_iter().collect())
    }

    fn result(&mut self, record_id: &str, code: &str, value: Decimal) {
        self.results
            .entry(record_id.to_owned())
            .or_default()
            .insert(code.to_owned(), value.normalize());
    }

    /// Writes each record's results that differ from what it holds.
    fn write(&mut self) -> Result<(), String> {
        let Some(context_id) = self.context_id.clone() else {
            return Ok(());
        };
        let results = std::mem::take(&mut self.results);
        for (record_id, values) in results {
            let record = self.record(&record_id)?;
            let changed: Vec<(String, Decimal)> = values
                .into_iter()
                .filter(|(code, value)| record.number(code, Some(&context_id)) != Some(*value))
                .collect();
            if changed.is_empty() {
                continue;
            }
            host::write_numbers(&record_id, &context_id, &changed)?;
            self.summary.records_written += 1;
        }
        Ok(())
    }
}

/// Orders the movements one save applied so that its production output
/// comes after everything else it applied. Completing an order consumes its
/// materials and receives its output in one transaction, in an order the
/// host does not define; scrap received first would lower the average the
/// materials leave at.
fn consumption_first(entries: Vec<LedgerEntry>, output: Option<&str>) -> Vec<LedgerEntry> {
    let Some(output) = output else {
        return entries;
    };
    let mut ordered = Vec::with_capacity(entries.len());
    let mut save: Vec<LedgerEntry> = Vec::new();
    for entry in entries {
        if save
            .first()
            .is_some_and(|first| first.applied_at != entry.applied_at)
        {
            flush(&mut ordered, &mut save, output);
        }
        save.push(entry);
    }
    flush(&mut ordered, &mut save, output);
    ordered
}

fn flush(ordered: &mut Vec<LedgerEntry>, save: &mut Vec<LedgerEntry>, output: &str) {
    let (outputs, others): (Vec<_>, Vec<_>) = save
        .drain(..)
        .partition(|entry| entry.record_blueprint == output);
    ordered.extend(others);
    ordered.extend(outputs);
}

/// The original a reversal record reverses, when it is one.
fn reversed(record: &RecordView, reverses: Option<&str>) -> Option<String> {
    record.target(reverses?)
}

fn nonzero(value: Decimal) -> Decimal {
    if value.is_zero() { Decimal::ONE } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(record: &str, blueprint: &str, applied_at: &str) -> LedgerEntry {
        LedgerEntry {
            record_id: record.into(),
            record_blueprint: blueprint.into(),
            applied_at: applied_at.into(),
            balance: "quantity".into(),
            amount: "1".into(),
            direction: "in".into(),
        }
    }

    #[test]
    fn a_saves_output_follows_its_consumption() {
        let ordered = consumption_first(
            vec![
                entry("pz", "receipt", "t1"),
                entry("scrap", "output", "t2"),
                entry("remnant", "output", "t2"),
                entry("material", "material", "t2"),
                entry("wz", "issue", "t3"),
            ],
            Some("output"),
        );
        let records: Vec<&str> = ordered.iter().map(|e| e.record_id.as_str()).collect();
        assert_eq!(records, ["pz", "material", "scrap", "remnant", "wz"]);
    }
}
