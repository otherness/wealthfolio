# Divergence ledger

Oracle baseline: `d3056da2a9b19c4f8f05a47b9258178db88ed1d5` (`main`, 2026-09-01) —
includes PR #1443, the authoritative-final-cash refactor (`f69936821` …
2026-08-28) and the market-sync-error fix (`6c2e3c573`).

Every legacy≠kernel delta is itemized here, maintainer-signed, and referenced
from the scenario it applies to. The parity harness skips only the itemized
deltas, never a whole scenario.

The parity harness (`crates/portfolio-engine/tests/parity.rs`) reads the
entries below and skips only the difference paths they name; everything else
must match. Paths are itemized to the field (`acc-1.performance.all_time.returns.twr`)
or to a whole section only when the section is absent on one side
(`acc-1.valuations`: legacy persisted no rows). `ledger_is_exact_and_consistent`
fails when a path matches no real difference or an `[L]` marker and an entry
disagree, or an entry is unsigned. `signed` is the date the entry was reviewed
and approved; a new entry starts as `pending` and the harness fails until it is
signed.

The legacy goldens were captured by a harness that only compiles against the
legacy calculators (deleted in the one-engine refactor); it is preserved
outside this repository (`legacy-oracle-capture`, commit `ac4da4edd`).
The goldens are frozen and never regenerated from this tree.

```yaml
- scenario: EDGE-CUR-01
  paths:
    - acc-1.flows
    - acc-1.keyframes[1].cash.
    - acc-1.keyframes[1].cash_total_account_currency
    - acc-1.keyframes[1].cash_total_base_currency
    - acc-1.keyframes[1].cost_basis
    - acc-1.keyframes[1].net_contribution_base
    - acc-1.keyframes[1].positions.aapl
    - acc-1.lots
    - acc-1.performance.all_time.attribution.contributions
    - acc-1.performance.all_time.attribution.unrealized_pnl_change
    - acc-1.performance.all_time.method
    - acc-1.performance.all_time.period_end
    - acc-1.performance.all_time.period_start
    - acc-1.performance.all_time.quality
    - acc-1.performance.all_time.returns.annualized_irr
    - acc-1.performance.all_time.returns.irr
    - acc-1.performance.all_time.returns.twr
    - acc-1.performance.all_time.returns.value_return
    - acc-1.performance.all_time.risk.drawdown_duration_days
    - acc-1.performance.all_time.risk.max_drawdown
    - acc-1.performance.all_time.risk.peak_date
    - acc-1.performance.all_time.risk.trough_date
    - acc-1.performance.all_time.risk.volatility
    - acc-1.performance.all_time.series
    - acc-1.performance.all_time.summary.amount
    - acc-1.performance.all_time.summary.amount_status
    - acc-1.performance.all_time.summary.basis
    - acc-1.performance.all_time.summary.method
    - acc-1.performance.all_time.summary.percent
    - acc-1.performance.all_time.summary.percent_status
    - acc-1.performance.all_time.summary.quality
    - acc-1.valuations
    - portfolio.all_time.attribution.contributions
    - portfolio.all_time.attribution.unrealized_pnl_change
    - portfolio.all_time.method
    - portfolio.all_time.period_end
    - portfolio.all_time.period_start
    - portfolio.all_time.quality
    - portfolio.all_time.returns.annualized_irr
    - portfolio.all_time.returns.irr
    - portfolio.all_time.returns.twr
    - portfolio.all_time.returns.value_return
    - portfolio.all_time.risk.drawdown_duration_days
    - portfolio.all_time.risk.max_drawdown
    - portfolio.all_time.risk.peak_date
    - portfolio.all_time.risk.trough_date
    - portfolio.all_time.risk.volatility
    - portfolio.all_time.series
    - portfolio.all_time.summary.amount
    - portfolio.all_time.summary.amount_status
    - portfolio.all_time.summary.basis
    - portfolio.all_time.summary.method
    - portfolio.all_time.summary.percent
    - portfolio.all_time.summary.percent_status
    - portfolio.all_time.summary.quality
    - portfolio_flows
  legacy: >-
    Books the empty-currency deposit into a "" cash bucket, adds it unconverted
    to the cash totals, and rejects the empty-currency BUY because the FX lookup
    fails on "" (no position, no lot, base contribution not updated); the
    valuation and performance then abort (noData).
  kernel: >-
    Assumes the account currency for the empty-currency rows (MissingCurrency
    diagnostic) and computes cash, position, lots, valuations, flows and
    performance normally.
  rationale: >-
    Issue #1388 shape; goal G5 (no silent bucket key). Kernel golden reviewed
    under EDGE-CUR-01.
  signed: 2026-09-02
- scenario: REG-1388
  paths:
    - acc-1.flows
    - acc-1.keyframes[1].cash.
    - acc-1.keyframes[1].cash.USD
    - acc-1.keyframes[1].net_contribution_base
    - acc-1.performance.all_time.attribution.contributions
    - acc-1.performance.all_time.method
    - acc-1.performance.all_time.period_end
    - acc-1.performance.all_time.period_start
    - acc-1.performance.all_time.quality
    - acc-1.performance.all_time.returns.annualized_irr
    - acc-1.performance.all_time.returns.irr
    - acc-1.performance.all_time.returns.twr
    - acc-1.performance.all_time.returns.value_return
    - acc-1.performance.all_time.risk.drawdown_duration_days
    - acc-1.performance.all_time.risk.max_drawdown
    - acc-1.performance.all_time.risk.peak_date
    - acc-1.performance.all_time.risk.trough_date
    - acc-1.performance.all_time.risk.volatility
    - acc-1.performance.all_time.series
    - acc-1.performance.all_time.summary.amount
    - acc-1.performance.all_time.summary.amount_status
    - acc-1.performance.all_time.summary.basis
    - acc-1.performance.all_time.summary.method
    - acc-1.performance.all_time.summary.percent
    - acc-1.performance.all_time.summary.percent_status
    - acc-1.performance.all_time.summary.quality
    - acc-1.valuations
    - portfolio.all_time.attribution.contributions
    - portfolio.all_time.method
    - portfolio.all_time.period_end
    - portfolio.all_time.period_start
    - portfolio.all_time.quality
    - portfolio.all_time.returns.annualized_irr
    - portfolio.all_time.returns.irr
    - portfolio.all_time.returns.twr
    - portfolio.all_time.returns.value_return
    - portfolio.all_time.risk.drawdown_duration_days
    - portfolio.all_time.risk.max_drawdown
    - portfolio.all_time.risk.peak_date
    - portfolio.all_time.risk.trough_date
    - portfolio.all_time.risk.volatility
    - portfolio.all_time.series
    - portfolio.all_time.summary.amount
    - portfolio.all_time.summary.amount_status
    - portfolio.all_time.summary.basis
    - portfolio.all_time.summary.method
    - portfolio.all_time.summary.percent
    - portfolio.all_time.summary.percent_status
    - portfolio.all_time.summary.quality
    - portfolio_flows
  legacy: >-
    Same mechanism as EDGE-CUR-01: the empty-currency deposit lands in a ""
    cash bucket, its base contribution is skipped and valuation aborts
    (noData). The kernel's 0% return is real: the scenario holds a deposit
    and no gain.
  kernel: >-
    Account-currency fallback with a MissingCurrency diagnostic (the #1388
    apply-path fix, now structural); valuations, flows and performance exist.
  rationale: >-
    Issue #1388.
  signed: 2026-09-02
- scenario: EDGE-CCY-02
  paths:
    - acc-lower.performance.all_time.attribution.contributions
    - acc-lower.performance.all_time.method
    - acc-lower.performance.all_time.period_end
    - acc-lower.performance.all_time.period_start
    - acc-lower.performance.all_time.quality
    - acc-lower.performance.all_time.series
    - acc-lower.performance.all_time.summary.basis
    - acc-lower.performance.all_time.summary.method
    - acc-lower.performance.all_time.summary.quality
    - acc-lower.valuations
    - portfolio.all_time.attribution.contributions
    - portfolio.all_time.attribution.unrealized_pnl_change
    - portfolio.all_time.quality
    - portfolio.all_time.returns.annualized_irr
    - portfolio.all_time.returns.irr
    - portfolio.all_time.returns.twr
    - portfolio.all_time.returns.value_return
    - portfolio.all_time.risk.drawdown_duration_days
    - portfolio.all_time.risk.max_drawdown
    - portfolio.all_time.risk.peak_date
    - portfolio.all_time.risk.trough_date
    - portfolio.all_time.risk.volatility
    - portfolio.all_time.series[6].value
    - portfolio.all_time.series[7].value
    - portfolio.all_time.series[8].value
    - portfolio.all_time.summary.amount
    - portfolio.all_time.summary.amount_status
    - portfolio.all_time.summary.percent
    - portfolio.all_time.summary.percent_status
    - portfolio.all_time.summary.quality
  legacy: >-
    Aborts the whole account valuation because the FX lookup for the lowercase
    `gbp` cash bucket fails ("Exchange rate not found: gbp->GBP"); no row is
    persisted, so acc-lower is silently absent from the portfolio scope and
    the portfolio reports a complete 0.12% return on the other account alone.
  kernel: >-
    `gbp` is an unknown currency: acc-lower is valued every day with
    `value_status: UNAVAILABLE` (base columns zero, a diagnostic names the
    bucket), and the portfolio scope, which now contains those days, reports
    its returns and headline amount as unavailable instead of silently
    excluding the account.
  rationale: >-
    Goal G5 (no silent bucket key, no all-or-nothing outage) and I10: a scope
    with an unavailable endpoint has no gain to report.
  signed: 2026-09-02
- scenario: EDGE-FX-04
  paths:
    - acc-1.performance.all_time.method
    - acc-1.performance.all_time.period_end
    - acc-1.performance.all_time.period_start
    - acc-1.performance.all_time.quality
    - acc-1.performance.all_time.series
    - acc-1.performance.all_time.summary.basis
    - acc-1.performance.all_time.summary.method
    - acc-1.performance.all_time.summary.quality
    - acc-1.valuations
    - portfolio.all_time.method
    - portfolio.all_time.period_end
    - portfolio.all_time.period_start
    - portfolio.all_time.series
    - portfolio.all_time.summary.basis
    - portfolio.all_time.summary.method
  legacy: >-
    No account->base rate at all: every day is skipped and the account gets
    no valuation rows (performance noData).
  kernel: >-
    Emits every day with `value_status: UNAVAILABLE`, account-currency columns
    filled, base columns zero, plus an FxUnavailable diagnostic. Performance
    keeps the period, method and series shape but every return and the
    headline amount are unavailable because both endpoints are UNAVAILABLE.
  rationale: >-
    architecture §4.3 (valuation never aborts; degradation is typed). Nothing numeric
    is reported that legacy did not report.
  signed: 2026-09-02
- scenario: EDGE-FX-05
  paths:
    - acc-1.performance.all_time.attribution.contributions
    - acc-1.performance.all_time.method
    - acc-1.performance.all_time.period_end
    - acc-1.performance.all_time.period_start
    - acc-1.performance.all_time.quality
    - acc-1.performance.all_time.series
    - acc-1.performance.all_time.summary.basis
    - acc-1.performance.all_time.summary.method
    - acc-1.performance.all_time.summary.quality
    - acc-1.valuations
    - portfolio.all_time.attribution.contributions
    - portfolio.all_time.method
    - portfolio.all_time.period_end
    - portfolio.all_time.period_start
    - portfolio.all_time.series
    - portfolio.all_time.summary.basis
    - portfolio.all_time.summary.method
  legacy: >-
    The CHF cash bucket cannot be converted, so the account valuation aborts
    and persists nothing (performance noData).
  kernel: >-
    Days are emitted as UNAVAILABLE with the convertible buckets valued and an
    FxUnavailable diagnostic naming the missing pair; contributions are known
    (700), returns and the headline amount are unavailable (UNAVAILABLE end
    point).
  rationale: >-
    Same as EDGE-FX-04.
  signed: 2026-09-02
- scenario: EDGE-FX-06
  paths:
    - acc-1.performance.all_time.attribution.contributions
    - acc-1.performance.all_time.method
    - acc-1.performance.all_time.period_end
    - acc-1.performance.all_time.period_start
    - acc-1.performance.all_time.quality
    - acc-1.performance.all_time.series
    - acc-1.performance.all_time.summary.basis
    - acc-1.performance.all_time.summary.method
    - acc-1.performance.all_time.summary.quality
    - acc-1.valuations
    - portfolio.all_time.attribution.contributions
    - portfolio.all_time.method
    - portfolio.all_time.period_end
    - portfolio.all_time.period_start
    - portfolio.all_time.series
    - portfolio.all_time.summary.basis
    - portfolio.all_time.summary.method
  legacy: >-
    The XYZ cash bucket cannot be converted, so the account valuation aborts
    (performance noData).
  kernel: >-
    Days are emitted as UNAVAILABLE with an FxUnavailable diagnostic;
    contributions known, returns and headline amount unavailable.
  rationale: >-
    Same as EDGE-FX-04.
  signed: 2026-09-02
- scenario: PERF-ATTR-02
  paths:
    - acc-div.performance.all_time.attribution.contributions
    - acc-div.performance.all_time.method
    - acc-div.performance.all_time.period_end
    - acc-div.performance.all_time.period_start
    - acc-div.performance.all_time.quality
    - acc-div.performance.all_time.risk.drawdown_duration_days
    - acc-div.performance.all_time.risk.max_drawdown
    - acc-div.performance.all_time.risk.peak_date
    - acc-div.performance.all_time.risk.trough_date
    - acc-div.performance.all_time.series
    - acc-div.performance.all_time.summary.basis
    - acc-div.performance.all_time.summary.method
    - acc-div.performance.all_time.summary.quality
    - acc-div.valuations
    - portfolio.all_time.attribution.contributions
    - portfolio.all_time.attribution.unrealized_pnl_change
    - portfolio.all_time.returns.annualized_irr
    - portfolio.all_time.returns.irr
    - portfolio.all_time.returns.twr
    - portfolio.all_time.returns.value_return
    - portfolio.all_time.risk.drawdown_duration_days
    - portfolio.all_time.risk.max_drawdown
    - portfolio.all_time.risk.recovery_date
    - portfolio.all_time.risk.volatility
    - portfolio.all_time.series[1].value
    - portfolio.all_time.series[2].value
    - portfolio.all_time.series[3].value
    - portfolio.all_time.series[4].value
    - portfolio.all_time.series[5].value
    - portfolio.all_time.summary.amount
    - portfolio.all_time.summary.amount_status
    - portfolio.all_time.summary.percent
    - portfolio.all_time.summary.percent_status
  legacy: >-
    The JPY dividend bucket has no JPY->USD rate, so acc-div is dropped from
    valuation entirely and silently from the portfolio scope, whose returns
    are then computed on acc-trade alone (4.25%, complete).
  kernel: >-
    acc-div is valued every day; days from the dividend onward are
    UNAVAILABLE with an FxUnavailable diagnostic. The portfolio scope contains
    those days, so its returns and headline amount are unavailable and its
    drawdown reflects the combined history.
  rationale: >-
    Same as EDGE-FX-04; PERF findings #6.
  signed: 2026-09-02
- scenario: PERF-FLOW-03
  paths:
    - acc-a.flows[0].outflow_base
    - acc-a.flows[0].source
    - acc-a.performance.acc_a_only.attribution.distributions
    - acc-a.performance.acc_a_only.quality
    - acc-a.performance.acc_a_only.returns.annualized_irr
    - acc-a.performance.acc_a_only.returns.irr
    - acc-a.performance.acc_a_only.returns.twr
    - acc-a.performance.acc_a_only.returns.value_return
    - acc-a.performance.acc_a_only.summary.percent
    - acc-a.performance.acc_a_only.summary.percent_status
    - acc-a.performance.acc_a_only.summary.quality
    - acc-a.performance.all_time.attribution.distributions
    - acc-a.performance.all_time.quality
    - acc-a.performance.all_time.returns.annualized_irr
    - acc-a.performance.all_time.returns.irr
    - acc-a.performance.all_time.returns.twr
    - acc-a.performance.all_time.returns.value_return
    - acc-a.performance.all_time.summary.percent
    - acc-a.performance.all_time.summary.percent_status
    - acc-a.performance.all_time.summary.quality
    - acc-a.valuations[2].external_flow_source
    - acc-a.valuations[2].external_outflow_base
    - portfolio.acc_a_only.attribution.distributions
    - portfolio.acc_a_only.quality
    - portfolio.acc_a_only.returns.annualized_irr
    - portfolio.acc_a_only.returns.irr
    - portfolio.acc_a_only.returns.twr
    - portfolio.acc_a_only.returns.value_return
    - portfolio.acc_a_only.summary.percent
    - portfolio.acc_a_only.summary.percent_status
    - portfolio.acc_a_only.summary.quality
    - portfolio.all_time.attribution.distributions
    - portfolio.all_time.quality
    - portfolio.all_time.returns.annualized_irr
    - portfolio.all_time.returns.irr
    - portfolio.all_time.returns.twr
    - portfolio.all_time.returns.value_return
    - portfolio.all_time.summary.percent
    - portfolio.all_time.summary.percent_status
    - portfolio.all_time.summary.quality
    - portfolio_flows[0].outflow_base
    - portfolio_flows[0].source
  legacy: >-
    The counterparty of the transfer is archived, so the activity read never
    sees it and the pair resolves to an UNKNOWN boundary (flow 0,
    UNKNOWN_BOUNDARY_TRANSFER, returns unavailable, quality partial).
  kernel: >-
    Archived accounts stay in the facts for pairing; the counterparty is out
    of scope, so the leg is an external CASH_AMOUNT outflow of 400 and the
    0% return is exact (the money left the tracked scope).
  rationale: >-
    architecture §4.2 (archived counterparty = money leaves the tracked scope).
  signed: 2026-09-02
- scenario: EDGE-QT-03
  paths:
    - acc-unavail.performance.all_time.attribution.unrealized_pnl_change
    - acc-unavail.performance.all_time.returns.value_return
    - acc-unavail.performance.all_time.summary.amount
    - acc-unavail.performance.all_time.summary.amount_status
    - portfolio.all_time.attribution.unrealized_pnl_change
    - portfolio.all_time.returns.value_return
    - portfolio.all_time.summary.amount
    - portfolio.all_time.summary.amount_status
  legacy: >-
    acc-unavail holds only an asset that has no quotes anywhere: legacy values
    the position at zero and reports a complete -100% value return and a -200
    amount (a total loss for an asset it could not price); the portfolio
    inherits -29.17% / -350.
  kernel: >-
    The rows are UNAVAILABLE (no priced position, no cash), so value return
    and the headline amount are unavailable on the account and on the
    portfolio scope, like TWR and IRR already were.
  rationale: >-
    I10 / architecture §4.3: no silent zero. Found by the 2026-09-02 adversarial
    review (silent-zero headlines over unavailable days).
  signed: 2026-09-02
```
