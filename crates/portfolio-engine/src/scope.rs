//! `facts_needed`: what a coordinator must load for a scope and range
//! (architecture §4.3 / §4.8). Pure over already-loaded account, asset and activity
//! facts; quote and FX observations are what it asks for.

use std::collections::BTreeSet;

use crate::model::*;

/// The facts a kernel run over `scope` × `range` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsRequest {
    /// The scope plus every transfer counterparty of a scoped activity
    /// (paired legs fold together and share the lot cache).
    pub accounts: BTreeSet<AccountId>,
    /// Assets referenced by those accounts' activities and observed
    /// snapshots.
    pub assets: BTreeSet<AssetId>,
    /// Currency pairs conversions may need (major codes, unordered pairs
    /// stored as `(from, to)` in both directions are not needed: the surface
    /// registers inverses).
    pub currency_pairs: BTreeSet<(String, String)>,
    /// Observation window. Loaders must add the latest observation on or
    /// before `range.start` per asset/pair (carry-forward seed) and, for FX,
    /// the nearest observation after `range.end` (nearest-neighbour
    /// resolution looks both ways).
    pub range: DateRange,
}

pub fn facts_needed(facts: &CanonicalFacts, scope: &[AccountId], range: DateRange) -> FactsRequest {
    let policy = &facts.policy;
    let base = policy
        .major_currency(policy.base_currency.as_str())
        .to_string();

    let mut accounts: BTreeSet<AccountId> = scope.iter().cloned().collect();
    for activity in &facts.activities {
        if !scope.contains(&activity.account) {
            continue;
        }
        if let Some(pair) = facts.transfer_pairs.pair_for(&activity.id) {
            accounts.insert(pair.out_account.clone());
            accounts.insert(pair.in_account.clone());
        }
    }

    let mut assets = BTreeSet::new();
    let mut currency_pairs = BTreeSet::new();
    let mut pair = |from: &str, to: &str| {
        let from = policy.major_currency(from).to_string();
        let to = policy.major_currency(to).to_string();
        if from != to {
            currency_pairs.insert((from, to));
        }
    };
    for account in accounts.iter().filter_map(|id| facts.accounts.get(id)) {
        pair(account.currency.as_str(), &base);
    }
    for activity in facts
        .activities
        .iter()
        .filter(|a| accounts.contains(&a.account))
    {
        if let Some(account) = facts.accounts.get(&activity.account) {
            pair(activity.currency.as_str(), account.currency.as_str());
        }
        pair(activity.currency.as_str(), &base);
        if let Some(asset) = &activity.asset {
            assets.insert(asset.clone());
        }
    }
    for snapshot in facts
        .observed_snapshots
        .iter()
        .filter(|s| accounts.contains(&s.account))
    {
        assets.extend(snapshot.positions.keys().cloned());
        if let Some(account) = facts.accounts.get(&snapshot.account) {
            for currency in snapshot.cash.keys() {
                pair(currency.as_str(), account.currency.as_str());
            }
        }
    }
    for asset in assets.iter().filter_map(|id| facts.assets.get(id)) {
        let Some(quote_currency) = &asset.quote_currency else {
            continue;
        };
        for account in accounts.iter().filter_map(|id| facts.accounts.get(id)) {
            pair(quote_currency.as_str(), account.currency.as_str());
        }
        pair(quote_currency.as_str(), &base);
    }

    FactsRequest {
        accounts,
        assets,
        currency_pairs,
        range,
    }
}
