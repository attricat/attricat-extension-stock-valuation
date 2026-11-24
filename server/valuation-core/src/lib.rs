//! Stock valuation without host calls: a moving weighted average replayed
//! over one stock item's movement history, and the allocation of a
//! production order's material cost to its output.
//!
//! Amounts are exact decimals. Values are rounded to the currency's minor
//! unit once per movement, and the movement that empties a stock item takes
//! all of its remaining value, so an empty item never keeps stray cents.

use std::collections::BTreeMap;

use rust_decimal::{Decimal, RoundingStrategy};

/// Decimal places of a unit cost, finer than the currency so that
/// `quantity × unit cost` reproduces a value closely.
pub const UNIT_COST_SCALE: u32 = 4;

/// Which way a movement changed the stock item's quantity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}

/// How a movement is valued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cost {
    /// A known total value, such as a receipt's `quantity × price` or what
    /// the original of a reversal moved.
    Value(Decimal),
    /// At the stock item's average cost when the movement applies, as an
    /// issue or a production consumption is.
    Average,
    /// What an earlier movement of this history moved, as the reversal of
    /// an issue returns it at the cost it left at. Unknown records fall
    /// back to the average.
    Same(String),
}

/// One movement of the stock item's quantity balance, in history order.
#[derive(Clone, Debug)]
pub struct Movement {
    /// The record whose movement this is, such as a goods issue.
    pub record_id: String,
    pub amount: Decimal,
    pub direction: Direction,
    pub cost: Cost,
}

/// What one movement moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Valued {
    pub value: Decimal,
    /// `value / amount`, at [`UNIT_COST_SCALE`].
    pub unit_cost: Decimal,
}

/// The result of replaying a stock item's history.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Replay {
    pub quantity: Decimal,
    pub value: Decimal,
    /// What each record's movement moved. A record moves one quantity
    /// balance once.
    pub movements: BTreeMap<String, Valued>,
}

impl Replay {
    /// The average cost per base unit, at [`UNIT_COST_SCALE`]; zero for an
    /// empty item.
    pub fn average_cost(&self) -> Decimal {
        unit_cost(self.value, self.quantity)
    }
}

/// `value / amount` at [`UNIT_COST_SCALE`], or zero without an amount.
pub fn unit_cost(value: Decimal, amount: Decimal) -> Decimal {
    if amount.is_zero() {
        Decimal::ZERO
    } else {
        round(value / amount, UNIT_COST_SCALE)
    }
}

/// Half away from zero, as invoices and Polish accounting round.
pub fn round(value: Decimal, scale: u32) -> Decimal {
    value
        .round_dp_with_strategy(scale, RoundingStrategy::MidpointAwayFromZero)
        .normalize()
}

/// Replays `movements` with a moving weighted average, rounding values to
/// `scale` decimal places.
pub fn replay(movements: &[Movement], scale: u32) -> Replay {
    let mut replayer = Replayer::new(scale);
    for movement in movements {
        replayer.step(movement);
    }
    replayer.finish()
}

/// A replay in progress, one movement at a time, so that a movement's cost
/// can depend on the movements before it, such as a remnant valued at the
/// material its order just consumed from the same stock item.
#[derive(Clone, Debug)]
pub struct Replayer {
    scale: u32,
    state: Replay,
}

impl Replayer {
    pub fn new(scale: u32) -> Self {
        Self {
            scale,
            state: Replay::default(),
        }
    }

    /// What has been replayed so far.
    pub fn state(&self) -> &Replay {
        &self.state
    }

    /// Applies one movement and returns what it moved.
    pub fn step(&mut self, movement: &Movement) -> Valued {
        let scale = self.scale;
        let state = &mut self.state;
        let cost = match &movement.cost {
            Cost::Same(original) => state
                .movements
                .get(original)
                .map_or(Cost::Average, |valued| Cost::Value(valued.value)),
            cost => cost.clone(),
        };
        let value = match (&cost, movement.direction) {
            (Cost::Same(_), _) => unreachable!("resolved above"),
            (Cost::Value(value), _) => round(*value, scale),
            (Cost::Average, Direction::In) => round(
                movement.amount * state.value / nonzero(state.quantity),
                scale,
            ),
            (Cost::Average, Direction::Out) => {
                if movement.amount >= state.quantity {
                    // The last unit out takes what is left.
                    state.value
                } else {
                    round(movement.amount * state.value / state.quantity, scale)
                }
            }
        };
        match movement.direction {
            Direction::In => {
                state.quantity += movement.amount;
                state.value += value;
            }
            Direction::Out => {
                state.quantity -= movement.amount;
                state.value -= value;
            }
        }
        if state.quantity.is_zero() {
            // A reversal or an explicit value can leave cents on an empty
            // item; they belong to the movement that emptied it.
            state.value = Decimal::ZERO;
        }
        let valued = Valued {
            value,
            unit_cost: unit_cost(value, movement.amount),
        };
        state
            .movements
            .insert(movement.record_id.clone(), valued.clone());
        valued
    }

    pub fn finish(mut self) -> Replay {
        self.state.quantity = self.state.quantity.normalize();
        self.state.value = self.state.value.normalize();
        self.state
    }
}

fn nonzero(value: Decimal) -> Decimal {
    if value.is_zero() { Decimal::ONE } else { value }
}

/// What a production output line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    Product,
    Remnant,
    Scrap,
}

impl OutputKind {
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "product" => Some(Self::Product),
            "remnant" => Some(Self::Remnant),
            "scrap" => Some(Self::Scrap),
            _ => None,
        }
    }
}

/// One output line of a production order.
#[derive(Clone, Debug)]
pub struct Output {
    pub record_id: String,
    pub kind: OutputKind,
    /// Base units received.
    pub amount: Decimal,
    /// Scrap: the buy-back price per base unit. Remnant: the cost per base
    /// unit of the material it is left from. Ignored for products.
    pub unit_price: Decimal,
}

/// A production order's cost split over its output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allocation {
    /// What the consumed materials cost.
    pub material_cost: Decimal,
    /// What the finished products carry: the material cost less scrap and
    /// remnants.
    pub production_cost: Decimal,
    pub outputs: BTreeMap<String, Valued>,
}

/// Values scrap and remnants at their own unit prices and gives the rest of
/// `material_cost` to the products, in proportion to their base quantity.
/// The last product takes the rounding difference, so the outputs add up to
/// the material cost exactly whenever there is a product.
pub fn allocate(material_cost: Decimal, outputs: &[Output], scale: u32) -> Allocation {
    let mut valued = BTreeMap::new();
    let mut by_products = material_cost;
    for output in outputs {
        if output.kind == OutputKind::Product {
            continue;
        }
        let value = round(output.amount * output.unit_price, scale);
        by_products -= value;
        valued.insert(
            output.record_id.clone(),
            Valued {
                value,
                unit_cost: unit_cost(value, output.amount),
            },
        );
    }
    let products: Vec<&Output> = outputs
        .iter()
        .filter(|output| output.kind == OutputKind::Product)
        .collect();
    let product_amount: Decimal = products.iter().map(|output| output.amount).sum();
    let mut left = by_products;
    for (index, output) in products.iter().enumerate() {
        let value = if index + 1 == products.len() {
            left
        } else {
            round(by_products * output.amount / nonzero(product_amount), scale)
        };
        left -= value;
        valued.insert(
            output.record_id.clone(),
            Valued {
                value,
                unit_cost: unit_cost(value, output.amount),
            },
        );
    }
    Allocation {
        material_cost: material_cost.normalize(),
        production_cost: if products.is_empty() {
            Decimal::ZERO
        } else {
            by_products.normalize()
        },
        outputs: valued,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(value: &str) -> Decimal {
        value.parse().unwrap()
    }

    fn receipt(id: &str, amount: &str, value: &str) -> Movement {
        Movement {
            record_id: id.into(),
            amount: d(amount),
            direction: Direction::In,
            cost: Cost::Value(d(value)),
        }
    }

    fn issue(id: &str, amount: &str) -> Movement {
        Movement {
            record_id: id.into(),
            amount: d(amount),
            direction: Direction::Out,
            cost: Cost::Average,
        }
    }

    #[test]
    fn issues_leave_at_the_moving_average() {
        // 1000 kg at 5.00 and 1000 kg at 6.00, then 500 kg out.
        let replay = replay(
            &[
                receipt("pz1", "1000", "5000"),
                receipt("pz2", "1000", "6000"),
                issue("wz1", "500"),
            ],
            2,
        );
        assert_eq!(replay.movements["wz1"].value, d("2750"));
        assert_eq!(replay.movements["wz1"].unit_cost, d("5.5"));
        assert_eq!(replay.quantity, d("1500"));
        assert_eq!(replay.value, d("8250"));
        assert_eq!(replay.average_cost(), d("5.5"));
    }

    #[test]
    fn the_last_unit_out_takes_the_remaining_value() {
        let replay = replay(
            &[
                receipt("pz1", "3", "10"),
                issue("wz1", "1"),
                issue("wz2", "1"),
                issue("wz3", "1"),
            ],
            2,
        );
        assert_eq!(replay.movements["wz1"].value, d("3.33"));
        assert_eq!(replay.movements["wz2"].value, d("3.34"));
        assert_eq!(replay.movements["wz3"].value, d("3.33"));
        assert_eq!(replay.value, Decimal::ZERO);
        assert_eq!(replay.quantity, Decimal::ZERO);
    }

    #[test]
    fn a_reversed_issue_returns_at_the_cost_it_left_at() {
        let reversal = Movement {
            record_id: "wz1-storno".into(),
            amount: d("4"),
            direction: Direction::In,
            cost: Cost::Same("wz1".into()),
        };
        let later = replay(
            &[
                receipt("pz1", "10", "50"),
                issue("wz1", "4"),
                receipt("pz2", "10", "80"),
                reversal,
            ],
            2,
        );
        assert_eq!(later.movements["wz1-storno"].value, d("20"));
        // 6 left at 5.00 (30), 10 in at 8.00 (110 in all), 4 back at 5.00.
        assert_eq!(later.quantity, d("20"));
        assert_eq!(later.value, d("130"));
    }

    #[test]
    fn production_cost_is_what_scrap_and_remnants_leave() {
        // 500 kg consumed for 2750; 400 kg product, 50 kg remnant at 5.50,
        // 50 kg scrap at 1.00.
        let allocation = allocate(
            d("2750"),
            &[
                Output {
                    record_id: "product".into(),
                    kind: OutputKind::Product,
                    amount: d("400"),
                    unit_price: Decimal::ZERO,
                },
                Output {
                    record_id: "remnant".into(),
                    kind: OutputKind::Remnant,
                    amount: d("50"),
                    unit_price: d("5.5"),
                },
                Output {
                    record_id: "scrap".into(),
                    kind: OutputKind::Scrap,
                    amount: d("50"),
                    unit_price: d("1"),
                },
            ],
            2,
        );
        assert_eq!(allocation.outputs["scrap"].value, d("50"));
        assert_eq!(allocation.outputs["remnant"].value, d("275"));
        assert_eq!(allocation.outputs["product"].value, d("2425"));
        assert_eq!(allocation.outputs["product"].unit_cost, d("6.0625"));
        assert_eq!(allocation.production_cost, d("2425"));
    }

    #[test]
    fn products_share_the_rest_by_quantity_and_add_up_exactly() {
        let product = |id: &str, amount: &str| Output {
            record_id: id.into(),
            kind: OutputKind::Product,
            amount: d(amount),
            unit_price: Decimal::ZERO,
        };
        let allocation = allocate(
            d("100"),
            &[product("a", "1"), product("b", "1"), product("c", "1")],
            2,
        );
        let values: Vec<Decimal> = ["a", "b", "c"]
            .iter()
            .map(|id| allocation.outputs[*id].value)
            .collect();
        assert_eq!(values, [d("33.33"), d("33.33"), d("33.34")]);
    }
}
