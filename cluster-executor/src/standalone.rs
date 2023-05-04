use std::collections::HashMap;
use std::time::Duration;

use log::{debug, info};
use remoc::rch::base::Sender;
use tokio_stream::StreamExt;

use overload_http::Request;

///! Standalone mode is a wrapper around cluster mode, instead of connecting to secondary nodes,
///! it connects localhost
use crate::{
    get_sender_for_host_port, log_error, remoc_port, send_end_msg, send_metadata_with_primary,
    send_request_to_secondary, JobStatus, MessageFromPrimary, RateMessage, RequestGenerator,
    JOB_STATUS, REMOC_PORT,
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

    //todo remove unwrap
    let sender = get_sender_for_host_port(*REMOC_PORT.get_or_init(remoc_port), "127.0.0.1")
        .await
        .unwrap();
    let mut senders = HashMap::with_capacity(1);
    senders.insert("localhost".to_string(), sender);
    let instances = ["localhost".to_string()];
    send_metadata_with_primary("127.0.0.1", &mut senders, &instances).await;
    let _ = send_request_to_secondary(request, &mut senders, &instances).await;
    {
        JOB_STATUS
            .write()
            .await
            .insert(job_id.clone(), JobStatus::InProgress);
    }
    let sender = senders.get_mut("localhost").unwrap();
    let mut counter = 0u8;
    while let Some((qps, connection_count)) = stream.next().await {
        counter += 1;
        if counter % 5 == 0 {
            // check for stop every 5 seconds
            if matches!(
                JOB_STATUS.read().await.get(&job_id),
                Some(&JobStatus::Stopped)
            ) {
                info!("[handle_request] - stopping job {}", &job_id);
                break;
            }
            counter = 0;
        }
        send_rate_message_to_executor(sender, qps, connection_count).await;
    }
    send_end_msg(sender, false).await;
    {
        JOB_STATUS
            .write()
            .await
            .insert(job_id, JobStatus::Completed);
    }
}

#[inline]
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
