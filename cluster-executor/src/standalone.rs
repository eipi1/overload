use std::time::Duration;

use log::debug;
use remoc::rch::base::Sender;
use tokio_stream::StreamExt;

use overload_http::Request;

///! Standalone mode is a wrapper around cluster mode, instead of connecting to secondary nodes,
///! it connects localhost
use crate::{
    get_sender_for_host_port, init_sender, log_error, remoc_port, send_end_msg, JobStatus,
    MessageFromPrimary, RateMessage, RequestGenerator, JOB_STATUS, REMOC_PORT,
};

pub async fn handle_request(request: Request) {
    let job_id = request.name.clone().unwrap();
    debug!(
        "[handle_request] - [{}] - handling request: {:?}",
        &job_id, &request
    );
    {
        JOB_STATUS
            .write()
            .await
            .insert(job_id.clone(), JobStatus::Starting);
    }
    let generator: RequestGenerator = request.clone().into();
    let stream = generator.throttle(Duration::from_secs(1));
    tokio::pin!(stream);

    let mut sender = get_sender_for_host_port(*REMOC_PORT.get_or_init(remoc_port), "127.0.0.1")
        .await
        .unwrap(); //todo remove unwrap
    let result = init_sender(request, "127.0.0.1".to_string(), &mut sender).await;
    log_error!(result);
    {
        JOB_STATUS
            .write()
            .await
            .insert(job_id.clone(), JobStatus::InProgress);
    }
    while let Some((qps, connection_count)) = stream.next().await {
        send_rate_message_to_executor(&mut sender, qps, connection_count).await;
    }
    send_end_msg(&mut sender, false).await;
    {
        JOB_STATUS
            .write()
            .await
            .insert(job_id, JobStatus::Completed);
    }
}

#[inline]
#[allow(dead_code)]
async fn send_rate_message_to_executor(
    sender: &mut Sender<MessageFromPrimary>,
    qps: u32,
    connection_count: u32,
) {
    let msg = MessageFromPrimary::Rates(RateMessage {
        qps,
        connections: connection_count,
    });
    let result = sender.send(msg).await;
    log_error!(result);
}
