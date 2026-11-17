// ABOUTME: Checks specification traceability before qualification runs.
// ABOUTME: Rejects dangling references and invalid requirement state.
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, serde::Serialize)]
pub struct CatalogSummary {
    pub requirements: usize,
    pub scenarios: usize,
    pub deferred: usize,
}
pub type CatalogError = String;
#[derive(Deserialize)]
struct Catalog {
    schema_version: u32,
    q25: Question,
    requirements: Vec<Requirement>,
    acceptance_scenarios: Vec<Scenario>,
}
#[derive(Deserialize)]
struct Question {
    selection: String,
    option_text: Option<String>,
}
#[derive(Deserialize)]
struct Requirement {
    id: String,
    milestone: String,
    acceptance_ids: Vec<String>,
    decision_ids: Vec<String>,
    depends_on: Vec<String>,
    implementation_status: String,
    verification_status: String,
}
#[derive(Deserialize)]
struct Scenario {
    id: String,
    requirement_ids: Vec<String>,
    status: String,
}
const VERIFICATION: &[&str] = &["not_run", "partial", "pass", "fail", "blocked"];

pub fn validate_catalog(bytes: &[u8]) -> Result<CatalogSummary, CatalogError> {
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("catalog exceeds 2 MiB".into());
    }
    let catalog: Catalog = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if catalog.schema_version != 1
        || catalog.q25.selection != "D"
        || catalog.q25.option_text.is_some()
    {
        return Err("unsupported schema or altered unresolved decision".into());
    }
    let requirements: BTreeMap<_, _> = catalog
        .requirements
        .iter()
        .map(|r| (r.id.as_str(), r))
        .collect();
    let scenarios: BTreeMap<_, _> = catalog
        .acceptance_scenarios
        .iter()
        .map(|s| (s.id.as_str(), s))
        .collect();
    if requirements.len() != catalog.requirements.len()
        || scenarios.len() != catalog.acceptance_scenarios.len()
    {
        return Err("duplicate catalog ID".into());
    }
    let decisions: BTreeSet<_> = (1..=24).map(|n| format!("D{n:02}")).collect();
    let mut referenced = BTreeSet::new();
    for r in &catalog.requirements {
        if r.id.is_empty()
            || !["M0", "M1", "M2", "M3", "M4", "LATER"].contains(&r.milestone.as_str())
            || !["not_started", "in_progress", "implemented"]
                .contains(&r.implementation_status.as_str())
            || !VERIFICATION.contains(&r.verification_status.as_str())
        {
            return Err(format!("invalid requirement state: {}", r.id));
        }
        for decision in &r.decision_ids {
            if !decisions.contains(decision) {
                return Err(format!("unknown decision: {decision}"));
            }
            referenced.insert(decision.clone());
        }
        for id in &r.acceptance_ids {
            if !scenarios
                .get(id.as_str())
                .is_some_and(|s| s.requirement_ids.contains(&r.id))
            {
                return Err(format!("unresolved or asymmetric scenario: {id}"));
            }
        }
        for id in &r.depends_on {
            if !requirements.contains_key(id.as_str()) {
                return Err(format!("unknown prerequisite: {id}"));
            }
        }
    }
    if referenced != decisions {
        return Err("missing decision coverage".into());
    }
    for s in &catalog.acceptance_scenarios {
        if s.id.is_empty() || !VERIFICATION.contains(&s.status.as_str()) {
            return Err(format!("invalid scenario state: {}", s.id));
        }
        for id in &s.requirement_ids {
            if !requirements
                .get(id.as_str())
                .is_some_and(|r| r.acceptance_ids.contains(&s.id))
            {
                return Err(format!("unresolved or asymmetric requirement: {id}"));
            }
        }
    }
    // Kahn's algorithm avoids recursive traversal of untrusted dependency depth.
    let mut remaining: BTreeSet<_> = requirements.keys().copied().collect();
    let mut visited = BTreeSet::new();
    while !remaining.is_empty() {
        let ready: Vec<_> = remaining
            .iter()
            .copied()
            .filter(|id| {
                requirements[id]
                    .depends_on
                    .iter()
                    .all(|d| visited.contains(d.as_str()))
            })
            .collect();
        if ready.is_empty() {
            return Err("requirement dependency cycle".into());
        }
        for id in ready {
            remaining.remove(id);
            visited.insert(id);
        }
    }
    Ok(CatalogSummary {
        requirements: requirements.len(),
        scenarios: scenarios.len(),
        deferred: catalog
            .requirements
            .iter()
            .filter(|r| r.milestone == "LATER")
            .count(),
    })
}
