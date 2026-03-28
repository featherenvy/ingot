pub fn parse_revision_context_summary(
    context: Option<&ingot_domain::revision_context::RevisionContext>,
) -> Option<ingot_domain::revision_context::RevisionContextSummary> {
    let context = context?;
    Some(ingot_domain::revision_context::RevisionContextSummary {
        updated_at: context.updated_at,
        changed_paths: context.payload.changed_paths.clone(),
        latest_validation: context.payload.latest_validation.clone(),
        latest_review: context.payload.latest_review.clone(),
        accepted_result_refs: context.payload.accepted_result_refs.clone(),
        operator_notes_excerpt: context.payload.operator_notes_excerpt.clone(),
    })
}
