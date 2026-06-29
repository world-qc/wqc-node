use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::application::ports::ResultSink;
use crate::application::state::AppState;
use crate::domain::models::{ComplexResult, ComputeRequest, ComputeTask, ExpectationResult, SampleResult, TaskResultPayload};
use crate::infra::core_client::WqcCoreClient;

struct TaskResultData {
    result_type: String,
    complex_result: ComplexResult,
    sample_result: Option<SampleResult>,
    expectation_result: Option<ExpectationResult>,
    proof: crate::domain::models::Proof,
    execution_time_ms: u64,
    work_report: Option<crate::domain::models::WorkReport>,
}

pub async fn start_worker(
    state: Arc<AppState>,
    result_sink: Arc<dyn ResultSink>,
    mut rx: mpsc::Receiver<ComputeTask>,
) {
    tracing::info!("Worker: Started task processing loop");

    while let Some(task) = rx.recv().await {
        process_task(state.clone(), result_sink.clone(), task).await;
    }
}

async fn process_task(
    state: Arc<AppState>,
    result_sink: Arc<dyn ResultSink>,
    task: ComputeTask,
) {
    let task_id = task.request.task_id.clone();
    let pubkey = task.orchestrator_pubkey.clone();

    tracing::info!("Worker: Starting task {}", task_id);

    let payload = match execute_compute(&state.core_client, &task.request).await {
        Ok(data) => TaskResultPayload {
            task_id: task_id.clone(),
            status: "success".to_string(),
            result_type: data.result_type,
            complex_result: Some(data.complex_result),
            sample_result: data.sample_result,
            expectation_result: data.expectation_result,
            proof: Some(data.proof),
            error: None,
            execution_time_ms: Some(data.execution_time_ms),
            work_report: data.work_report,
        },
        Err(e) => {
            tracing::error!("Task {} failed: {}", task_id, e);
            TaskResultPayload {
                task_id: task_id.clone(),
                status: "error".to_string(),
                result_type: String::new(),
                complex_result: None,
                sample_result: None,
                expectation_result: None,
                proof: None,
                error: Some(e.to_string()),
                execution_time_ms: None,
                work_report: None,
            }
        }
    };

    let status = if payload.status == "error" { "failed" } else { "completed" };
    if let Err(e) = state.storage.update_status(&pubkey, &task_id, status) {
        tracing::error!("Storage update failed for task {} owned by {}: {}", task_id, pubkey, e);
    }

    if let Err(e) = result_sink.send_result(&task, payload).await {
        tracing::error!("Result delivery failed for task {}: {}", task_id, e);
    }

    state.pending_tasks.fetch_sub(1, Ordering::SeqCst);
    tracing::info!("Worker: Finished task {}", task_id);
}

async fn execute_compute(
    core_client: &WqcCoreClient,
    request: &ComputeRequest,
) -> anyhow::Result<TaskResultData> {
    let start_time = std::time::Instant::now();
    let res = core_client.dispatch_task(request.clone()).await?;
    let execution_time_ms = start_time.elapsed().as_millis() as u64;

    Ok(TaskResultData {
        result_type: if res.result_type.is_empty() {
            "statevector_scalar".to_string()
        } else {
            res.result_type
        },
        complex_result: res.complex_result,
        sample_result: res.sample_result,
        expectation_result: res.expectation_result,
        proof: res.proof,
        execution_time_ms,
        work_report: res.work_report,
    })
}
