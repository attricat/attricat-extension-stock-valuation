# attricat-stock-valuation

An Attricat extension that values stock. It keeps each stock item's value
and average cost at a **moving weighted average**, values every issue and
production consumption at the average when it applied, and values a
production order's output from the materials it consumed.

It is a derived view: the extension replays a stock item's movement history
(`attricat.read.v1` `movements`) from the start whenever the item's balance
changes or a receipt's price is entered, and writes the results that
changed. Results therefore appear a few seconds after the save, and the same
history always gives the same values, however events were ordered or
repeated.

## How values are computed

| Movement | Value |
| --- | --- |
| Receipt | `quantity × unit price × exchange rate`; a receipt without a price is valued at zero and logged |
| Issue, production consumption | the stock item's average cost when it applied |
| Reversal (of a receipt or an issue) | what the original moved |
| Production output | its share of the order's material cost (below) |

Values are rounded to `decimal_places` (default 2, the currency's minor unit)
half away from zero, once per movement. The movement that empties a stock
item takes all of its remaining value, so an empty item keeps no stray cents.
Unit costs are kept at 4 decimal places.

A completed production order's **material cost** is the sum of its consumed
material lines. Scrap is valued at its item's buy-back price per base unit
(`scrap_price`), remnants at the average cost of the material of the same
item the order consumed, and the finished products share what is left in
proportion to their base quantity. That remainder is the order's
**production cost**. When a material's cost changes, the products made from
it are revalued too.

## Configuration

The installation configuration names the blueprints and attributes; every
attribute code has a default matching the Inventory solution pack. An empty
configuration turns valuation off.

```json
{
  "decimal_places": 2,
  "stock_item": {"blueprint": "inv_stock_item"},
  "receipts": [{"blueprint": "inv_goods_receipt", "unit_price": "unit_price_net"}],
  "issues": [{"blueprint": "inv_goods_issue"}],
  "production": {
    "order": {"blueprint": "inv_production_order"},
    "material": {"blueprint": "inv_production_material"},
    "output": {"blueprint": "inv_production_output"}
  }
}
```

The attributes the extension writes (`value`, `unit_cost`, `average_cost`,
`material_cost`, `production_cost`) should be `readonly: true` numbers in
the default context. A status lock on a completed order must leave the cost
attributes out of its `lock` list, or the write is refused.

## Surfaces

| Surface | Use |
| --- | --- |
| `server.event_handlers` | `record.updated.v1` of a stock item, a configured movement record or an order revalues the stock items concerned; events caused by the extension's own writes are skipped |
| `server.commands` + `record_action` | **Recalculate valuation** (Polish: *Przelicz wycenę*) on a stock item, a document or an order |
| `attricat.read.v1` | `movements` for a stock item's history, `incoming` for an order's lines |

## Development

```sh
just check   # fmt, clippy, unit tests
just pack    # dist/attricat-stock-valuation-<version>.tar.zst
```

The component targets the `outcome-extension` world of
`attricat:host@1.1.0`; `server/component/wit` is the SDK's released snapshot,
copied verbatim. The `movements` and `incoming` reads need an Attricat host
that has them (core `main` after 1.1.1).

To verify against a running Attricat development server: side-load the
archive (`acli extension sideload`), grant its permissions, set the
configuration, enable it, then post a receipt with a price, an issue and a
completed production order and check the stock items' `value`.

MIT licensed; see `LICENSE`.
