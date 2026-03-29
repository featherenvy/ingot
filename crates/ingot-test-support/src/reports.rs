use serde_json::Value;

pub fn clean_review_report(base_commit_oid: &str, head_commit_oid: &str) -> Value {
    ingot_agent_protocol::report::clean_review_report_payload(base_commit_oid, head_commit_oid)
}

pub fn findings_review_report(
    base_commit_oid: &str,
    head_commit_oid: &str,
    summary: &str,
    overall_risk: &str,
    findings: Vec<Value>,
) -> Value {
    ingot_agent_protocol::report::findings_review_report_payload(
        base_commit_oid,
        head_commit_oid,
        summary,
        overall_risk,
        findings,
    )
}

pub fn clean_validation_report(summary: &str) -> Value {
    ingot_agent_protocol::report::clean_validation_report_payload(summary)
}
