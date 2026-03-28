use crate::convergence::Convergence;
use crate::convergence_queue::ConvergenceQueueEntry;
use crate::finding::Finding;
use crate::item::Item;
use crate::job::Job;
use crate::project::Project;
use crate::revision::ItemRevision;

#[derive(Debug, Clone)]
pub struct ConvergenceQueuePrepareContext {
    pub project: Project,
    pub item: Item,
    pub revision: ItemRevision,
    pub jobs: Vec<Job>,
    pub findings: Vec<Finding>,
    pub convergences: Vec<Convergence>,
    pub active_queue_entry: Option<ConvergenceQueueEntry>,
    pub lane_head: Option<ConvergenceQueueEntry>,
}
