use serde_json::{Value, json};

pub fn clean_review_report(base_commit_oid: &str, head_commit_oid: &str) -> Value {
    json!({
        "outcome": "clean",
        "summary": "No issues found",
        "review_subject": {
            "base_commit_oid": base_commit_oid,
            "head_commit_oid": head_commit_oid
        },
        "overall_risk": "low",
        "findings": []
    })
}

pub fn findings_review_report(
    base_commit_oid: &str,
    head_commit_oid: &str,
    summary: &str,
    overall_risk: &str,
    findings: Vec<Value>,
) -> Value {
    json!({
        "outcome": "findings",
        "summary": summary,
        "review_subject": {
            "base_commit_oid": base_commit_oid,
            "head_commit_oid": head_commit_oid
        },
        "overall_risk": overall_risk,
        "findings": findings
    })
}

pub fn clean_validation_report(summary: &str) -> Value {
    json!({
        "outcome": "clean",
        "summary": summary,
        "checks": [],
        "findings": []
    })
}
