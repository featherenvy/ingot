use super::convergence_port::HttpConvergencePort;
use super::deps::*;
use ingot_usecases::convergence::RejectApprovalTeardown;

#[derive(Clone)]
pub(super) struct HttpConvergenceRouteAdapter {
    state: AppState,
}

impl HttpConvergenceRouteAdapter {
    pub(super) fn new(state: &AppState) -> Self {
        Self {
            state: state.clone(),
        }
    }

    fn service(&self) -> ConvergenceService<HttpConvergencePort> {
        ConvergenceService::new(HttpConvergencePort::new(&self.state))
    }

    pub(super) async fn queue_prepare(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> Result<(), ApiError> {
        self.service()
            .queue_prepare(project_id, item_id)
            .await
            .map_err(ApiError::from)
    }

    pub(super) async fn approve_item(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> Result<(), ApiError> {
        self.service()
            .approve_item(project_id, item_id)
            .await
            .map_err(ApiError::from)
    }

    pub(super) async fn reject_item_approval(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
        next_revision: &ItemRevision,
    ) -> Result<RejectApprovalTeardown, ApiError> {
        self.service()
            .reject_item_approval(project_id, item_id, next_revision)
            .await
            .map_err(ApiError::from)
    }
}
