use super::*;

impl JobDispatcher {
    pub(super) async fn write_prompt_artifact(
        &self,
        job: &Job,
        prompt: &str,
    ) -> Result<(), RuntimeError> {
        let dir = self.artifact_dir(job.id);
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::write(dir.join("prompt.txt"), prompt).await?;
        Ok(())
    }

    pub(super) async fn write_response_artifacts(
        &self,
        job: &Job,
        response: &AgentResponse,
    ) -> Result<(), RuntimeError> {
        let dir = self.artifact_dir(job.id);
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::write(dir.join("stdout.log"), &response.stdout).await?;
        tokio::fs::write(dir.join("stderr.log"), &response.stderr).await?;
        let result_json = response.result.clone().unwrap_or(serde_json::Value::Null);
        tokio::fs::write(
            dir.join("result.json"),
            serde_json::to_vec_pretty(&result_json)?,
        )
        .await?;
        Ok(())
    }

    fn artifact_dir(&self, job_id: ingot_domain::ids::JobId) -> PathBuf {
        self.config.state_root.join("logs").join(job_id.to_string())
    }
}
