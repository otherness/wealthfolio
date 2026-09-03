//! Projection parity: kernel `normalize → compile → resolve → project` versus
//! the legacy goldens captured by `crates/core` (architecture §5 harness 3, step
//! 2.1). Every parity-eligible scenario with a legacy golden must match on
//! keyframes, lot rows and disposals — or be itemized in DIVERGENCES.md.

mod support;

use std::collections::BTreeMap;

use serde_json::Value;
use support::*;
use wealthfolio_portfolio_engine::model::*;

/// Every kernel≠legacy difference for one scenario, before the ledger.
fn scenario_differences(scenario: &Scenario, golden: &Value) -> Vec<String> {
    let golden = golden.clone();
    let pipeline = Pipeline::from_scenario(scenario);
    let (facts, bundle, series) = (&pipeline.facts, &pipeline.bundle, &pipeline.series);
    let fx = pipeline.fx();
    let inputs = pipeline.value_inputs();
    let lots = pipeline.lots();
    let measure_inputs = pipeline.measure_inputs(&lots);
    let windows = all_windows(scenario);
    let portfolio_scope = pipeline.portfolio_scope();

    let mut differences = Vec::new();
    for (account_id, account) in &facts.accounts {
        if account.archived {
            continue;
        }
        if account.tracking != TrackingMode::Holdings {
            let Some(legacy) = legacy_projection(&golden, account_id.as_str()) else {
                differences.push(format!("{account_id}: missing in legacy golden"));
                continue;
            };
            let kernel = capture_projection(bundle, account_id, facts, &fx);
            let left = serde_json::to_value(&kernel).unwrap();
            let right = serde_json::to_value(&legacy).unwrap();
            diff_values(account_id.as_str(), &left, &right, &mut differences);
        }
        let Some(legacy) = legacy_valuation(&golden, account_id.as_str()) else {
            continue;
        };
        let kernel = match capture_valuation(&inputs, series, account_id) {
            Ok(capture) => capture,
            Err(error) => {
                differences.push(format!("{account_id}.flows: error: {error}"));
                continue;
            }
        };
        let left = serde_json::to_value(&kernel).unwrap();
        let right = serde_json::to_value(&legacy).unwrap();
        diff_values(account_id.as_str(), &left, &right, &mut differences);

        for window in &windows {
            let in_scope = window
                .accounts
                .as_ref()
                .is_none_or(|ids| ids.iter().any(|id| id == account_id.as_str()));
            if !in_scope {
                continue;
            }
            let Some(legacy) =
                legacy_account_performance(&golden, account_id.as_str(), &window.label)
            else {
                continue;
            };
            let kernel = capture_account_performance(&measure_inputs, account_id, window);
            diff_values(
                &format!("{account_id}.performance.{}", window.label),
                &kernel,
                &legacy,
                &mut differences,
            );
        }
    }
    for window in &windows {
        let scope: Vec<AccountId> = window
            .accounts
            .as_ref()
            .map(|ids| ids.iter().map(|id| AccountId::new(id.as_str())).collect())
            .unwrap_or_else(|| portfolio_scope.clone());
        if scope.is_empty() {
            continue;
        }
        let Some(legacy) = legacy_portfolio_performance(&golden, &window.label) else {
            continue;
        };
        let kernel = capture_scope_performance(&measure_inputs, &scope, window);
        diff_values(
            &format!("portfolio.{}", window.label),
            &kernel,
            &legacy,
            &mut differences,
        );
    }
    match capture_portfolio_flows(&inputs, series) {
        Ok(flows) => diff_values(
            "portfolio_flows",
            &Value::Array(flows),
            &Value::Array(legacy_portfolio_flows(&golden)),
            &mut differences,
        ),
        Err(error) => differences.push(format!("portfolio_flows: error: {error}")),
    }
    differences
}

#[test]
fn projection_matches_legacy_goldens() {
    let divergences = load_ledger();
    let legacy_goldens = legacy_golden_count();
    let mut failures: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut ledgered: BTreeMap<String, usize> = BTreeMap::new();
    let mut compared = 0;
    let filtered = std::env::var_os("SCENARIO_FILTER").is_some();
    for scenario in load_all_scenarios() {
        if !scenario.is_parity_eligible() || !scenario_selected(&scenario.id) {
            continue;
        }
        let Some(golden) = load_legacy_golden(&scenario.id) else {
            continue;
        };
        compared += 1;
        let differences = scenario_differences(&scenario, &golden);
        if std::env::var_os("PARITY_DUMP").is_some() {
            for line in &differences {
                eprintln!("{}: {line}", scenario.id);
            }
        }
        let (unledgered, sanctioned) = apply_ledger(&scenario.id, differences, &divergences);
        if !sanctioned.is_empty() {
            ledgered.insert(scenario.id.clone(), sanctioned.len());
        }
        if !unledgered.is_empty() {
            failures.insert(scenario.id.clone(), unledgered);
        }
    }
    if filtered {
        assert!(compared > 0, "SCENARIO_FILTER selected no legacy golden");
    } else {
        assert_eq!(
            compared, legacy_goldens,
            "every legacy golden must be compared (parity must not narrow silently)"
        );
    }
    for (id, count) in &ledgered {
        eprintln!("{id}: {count} ledgered difference(s)");
    }
    if !failures.is_empty() {
        let mut report = String::new();
        for (id, diffs) in &failures {
            report.push_str(&format!("\n== {id} ({} differences)\n", diffs.len()));
            for line in diffs.iter().take(25) {
                report.push_str(&format!("  {line}\n"));
            }
        }
        panic!(
            "projection parity failed for {} of {compared} scenarios:{report}",
            failures.len()
        );
    }
}

/// The ledger is exact: every path it names matches a real difference (no
/// stale or over-broad prefixes), and `[L]` markers and entries agree.
#[test]
fn ledger_is_exact_and_consistent() {
    let divergences = load_ledger();
    let mut problems = Vec::new();
    let scenarios = load_all_scenarios();
    for scenario in &scenarios {
        let has_entry = divergences.iter().any(|e| e.scenario == scenario.id);
        let marked = scenario.markers.iter().any(|m| m == "L");
        if marked && !has_entry {
            problems.push(format!(
                "{}: marked [L] but has no ledger entry",
                scenario.id
            ));
        }
        if has_entry && !marked {
            problems.push(format!(
                "{}: has a ledger entry but no [L] marker",
                scenario.id
            ));
        }
        let Some(golden) = load_legacy_golden(&scenario.id) else {
            if has_entry {
                problems.push(format!(
                    "{}: ledger entry without a legacy golden",
                    scenario.id
                ));
            }
            continue;
        };
        if !scenario.is_parity_eligible() {
            continue;
        }
        let differences = scenario_differences(scenario, &golden);
        for entry in divergences.iter().filter(|e| e.scenario == scenario.id) {
            // Every entry is a maintainer decision; an unsigned or pending
            // entry must not silently sanction a divergence.
            let signed = entry.signed.trim();
            if signed.is_empty() || signed.eq_ignore_ascii_case("pending") {
                problems.push(format!("{}: ledger entry is not signed", scenario.id));
            }
            for path in &entry.paths {
                if !differences.iter().any(|d| d.starts_with(path.as_str())) {
                    problems.push(format!(
                        "{}: ledger path {path:?} matches no difference",
                        scenario.id
                    ));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "ledger problems:\n  {}",
        problems.join("\n  ")
    );
}
