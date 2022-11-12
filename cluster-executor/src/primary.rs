use crate::{
    get_sender_for_host_port, init_sender, remoc_port_name, send_end_msg, JobStatus,
    MessageFromPrimary, RateMessage, RequestGenerator, JOB_STATUS, REMOC_PORT_NAME,
};
use cluster_mode::{Cluster, RestClusterNode};
use log::{debug, error, info};
use overload_http::Request;
use remoc::rch::base::Sender;
use rust_cloud_discovery::ServiceInstance;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::StreamExt;

pub async fn handle_request(request: Request, cluster: Arc<Cluster>) {
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
    let mut counter = 0u8;
    let mut senders = HashMap::new();
    {
        let request = request.clone();
        init_senders(&mut senders, &cluster, request).await;
    }
    {
        JOB_STATUS
            .write()
            .await
            .insert(job_id.clone(), JobStatus::InProgress);
    }
    while let Some((qps, connection_count)) = stream.next().await {
        if counter % 5 == 0 {
            // check for stop every 5 seconds
            if matches!(
                JOB_STATUS.read().await.get(&job_id),
                Some(&JobStatus::Stopped)
            ) {
                info!("[handle_request] - stopping job {}", &job_id);
                break;
            }
        }
        // update secondary list every 10 seconds
        if counter == 10 {
            {
                let request = request.clone();
                init_senders(&mut senders, &cluster, request).await;
            }
            counter = 0;
        } else {
            counter += 1;
        }

        debug!(
            "[handle_request] [{}] - sending secondaries - qps:{:?}, connections:{:?}",
            &job_id, qps, connection_count
        );

        let result = send_rate_message_to_secondaries(&mut senders, qps, connection_count).await;
        if let Err(e) = result {
            //remove failed senders
            for instance_id in e {
                debug!(
                    "[handle_request] - dropping sender to instance: {}",
                    &instance_id
                );
                senders.remove(&instance_id);
            }
        }
    }

    for (instance, mut sender) in senders.drain() {
        debug!("[handle_request] - sending end message to: {}", instance);
        send_end_msg(&mut sender, false).await;
    }
    JOB_STATUS
        .write()
        .await
        .insert(job_id, JobStatus::Completed);
}

/// Send rate message to secondaries.
/// # Returns
/// Ok if all message sent successfully to all senders or list of instance ids of those failed.
#[inline]
async fn send_rate_message_to_secondaries(
    senders: &mut HashMap<String, Sender<MessageFromPrimary>>,
    qps: u32,
    connection_count: u32,
) -> Result<(), Vec<String>> {
    let (qps_to_secondaries, conn_count_to_secondaries) =
        calculate_req_per_secondary(qps, connection_count, senders.len());
    let mut failed_senders = vec![];
    for (pos, (instance_id, sender)) in senders.iter_mut().enumerate() {
        let msg = MessageFromPrimary::Rates(RateMessage {
            qps: qps_to_secondaries[pos],
            connections: conn_count_to_secondaries[pos],
        });
        if let Err(e) = sender.send(msg).await {
            error!(
                "[send_rate_message_to_secondaries] - error while sending msg: {:?}",
                &e
            );
            failed_senders.push(instance_id.clone());
        }
    }
    if failed_senders.is_empty() {
        Ok(())
    } else {
        Err(failed_senders)
    }
}

fn calculate_req_per_secondary(
    qps: u32,
    connection_count: u32,
    n_secondary: usize,
) -> (Vec<u32>, Vec<u32>) {
    let qps_per_secondary = qps / n_secondary as u32;
    let qps_remainder = qps as usize % n_secondary;

    let connection_count_per_secondary = connection_count / n_secondary as u32;
    let connection_count_remainder = connection_count as usize % n_secondary;

    let mut qps_to_secondaries = vec![qps_per_secondary; n_secondary];
    let mut connection_count_to_secondaries = vec![connection_count_per_secondary; n_secondary];

    let zip = qps_to_secondaries
        .iter_mut()
        .zip(connection_count_to_secondaries.iter_mut());
    for (pos, (qps_, conn_con_)) in zip.enumerate() {
        if pos < qps_remainder {
            *qps_ += 1;
        }
        if pos < connection_count_remainder {
            *conn_con_ += 1;
        }
    }

    (qps_to_secondaries, connection_count_to_secondaries)
}

#[inline]
async fn init_senders(
    senders: &mut HashMap<String, Sender<MessageFromPrimary>>,
    cluster: &Cluster,
    request: Request,
) {
    if let Some(secondaries) = cluster.secondaries().await {
        let primary = cluster.get_service_instance().await.unwrap();
        init_senders_for_cluster_nodes(senders, &secondaries, &primary, request).await;
    }
}

async fn init_senders_for_cluster_nodes(
    senders: &mut HashMap<String, Sender<MessageFromPrimary>>,
    secondaries: &HashSet<RestClusterNode>,
    primary: &ServiceInstance,
    request: Request,
) {
    let primary_host = primary.host().as_ref().unwrap().clone();
    for secondary in secondaries.iter() {
        let instance_id: &Option<String> = secondary.service_instance().instance_id();
        if senders.get(instance_id.as_ref().unwrap()).is_none() {
            if let Some(mut sender) = get_sender_for_secondary(secondary).await {
                let result = {
                    let request = request.clone();
                    init_sender(request, primary_host.clone(), &mut sender).await
                };
                match result {
                    Ok(_) => {
                        senders.insert(instance_id.as_ref().cloned().unwrap(), sender);
                    }
                    Err(e) => {
                        error!(
                            "[init_senders_for_cluster_nodes] - error initializing sender: {:?}",
                            e
                        )
                    }
                }
            }
        }
    }
}

pub(crate) async fn get_sender_for_secondary(
    secondary: &RestClusterNode,
) -> Option<Sender<MessageFromPrimary>> {
    let instance = secondary.service_instance();
    let ports = instance.get_ports().as_ref()?;
    let port: u32 = ports
        .iter()
        .find(|p| p.get_name().as_deref() == Some(REMOC_PORT_NAME.get_or_init(remoc_port_name)))?
        .get_port();
    let host: &str = instance.host().as_ref()?.as_str();
    get_sender_for_host_port(port as u16, host).await
}

#[cfg(test)]
mod test {
    use crate::primary::{get_sender_for_secondary, init_sender, send_rate_message_to_secondaries};
    use crate::test_common::{cluster_node, get_request, init};
    use crate::{log_error, MessageFromPrimary};
    use log::trace;
    use remoc::rch;
    use remoc::rch::base::Receiver;
    use remoc::rtc::async_trait;
    use rust_cloud_discovery::{DiscoveryService, ServiceInstance};
    use std::collections::HashMap;
    use std::error::Error;
    use std::net::Ipv4Addr;
    use std::sync::mpsc::{channel, Receiver as StdReceiver, Sender as StdSender};
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::time::sleep;

    async fn start_tcp_listener_random_port() -> (TcpListener, u16) {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        trace!("started listener at {}", port);
        (listener, port)
    }

    async fn start_server_with_listener(
        listener: TcpListener,
        sender: StdSender<MessageFromPrimary>,
    ) {
        loop {
            let sender_c = sender.clone();
            let (socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let (socket_rx, socket_tx) = socket.into_split();
                let (conn, _tx, mut rx): (_, rch::base::Sender<()>, Receiver<MessageFromPrimary>) =
                    remoc::Connect::io(remoc::Cfg::default(), socket_rx, socket_tx)
                        .await
                        .unwrap();
                tokio::spawn(conn);
                trace!("Starting client");
                loop {
                    let result = rx.recv().await;
                    println!("{:?}", &result);
                    let end = result
                        .into_iter()
                        .flatten()
                        .map(|msg| {
                            let end = matches!(
                                msg,
                                MessageFromPrimary::Stop | MessageFromPrimary::Finished
                            );
                            let _ = sender_c.send(msg);
                            end
                        })
                        .last()
                        .unwrap_or_default();
                    if end {
                        break;
                    }
                }
            });
        }
    }

    #[tokio::test(flavor = "multi_thread")] //requires multi thread
    async fn test_send_message_to_secondaries() {
        init();
        let (tx, rx1) = channel();
        let (listener, port) = start_tcp_listener_random_port().await;
        tokio::spawn(start_server_with_listener(listener, tx));
        // sleep(Duration::from_millis(5)).await;

        let mut sender_map = HashMap::new();

        let node = cluster_node(3030, port as u32);
        let mut sender = get_sender_for_secondary(&node).await.unwrap();
        let result = init_sender(
            get_request("localhost".to_string(), 8082),
            "localhost".to_string(),
            &mut sender,
        )
        .await;
        log_error!(result);
        let id: String = node
            .service_instance()
            .instance_id()
            .as_ref()
            .unwrap()
            .clone();
        sender_map.insert(id, sender);

        // expect a message with metadata, and then request
        let msg = rx1.recv_timeout(Duration::from_millis(500)).unwrap();
        assert!(matches!(msg, MessageFromPrimary::Metadata(_)));
        let msg = rx1.recv_timeout(Duration::from_millis(500)).unwrap();
        assert!(matches!(msg, MessageFromPrimary::Request(_)));

        let _ = send_rate_message_to_secondaries(&mut sender_map, 10, 5).await;
        let primary = rx1.recv_timeout(Duration::from_millis(500)).unwrap();
        match primary {
            MessageFromPrimary::Rates(r) => {
                assert_eq!(r.connections, 5);
                assert_eq!(r.qps, 10);
            }
            _ => {
                panic!("unexpected message");
            }
        }

        let (tx2, rx2) = channel();
        let (listener2, port2) = start_tcp_listener_random_port().await;
        tokio::spawn(start_server_with_listener(listener2, tx2));

        let node2 = cluster_node(3030, port2 as u32);
        let mut sender = get_sender_for_secondary(&node2).await.unwrap();
        let _ = init_sender(
            get_request("localhost".to_string(), 8082),
            "localhost".to_string(),
            &mut sender,
        )
        .await;

        //verify init request for node 2, expect metadata, then request
        let msg = rx2.recv_timeout(Duration::from_millis(500)).unwrap();
        assert!(matches!(msg, MessageFromPrimary::Metadata(_)));
        let msg = rx2.recv_timeout(Duration::from_millis(500)).unwrap();
        assert!(matches!(msg, MessageFromPrimary::Request(_)));

        let id: String = node2
            .service_instance()
            .instance_id()
            .as_ref()
            .unwrap()
            .clone();
        sender_map.insert(id, sender);

        let _ = send_rate_message_to_secondaries(&mut sender_map, 10, 5).await;
        let mut qps_count = 0;
        let mut conn_count = 0;
        assert_rate_msg(&rx1, &mut qps_count, &mut conn_count, vec![5], vec![2, 3]);
        assert_rate_msg(&rx2, &mut qps_count, &mut conn_count, vec![5], vec![2, 3]);
        assert_eq!(qps_count, 10);
        assert_eq!(conn_count, 5);

        let _ = send_rate_message_to_secondaries(&mut sender_map, 5, 10).await;
        let mut qps_count = 0;
        let mut conn_count = 0;
        assert_rate_msg(&rx1, &mut qps_count, &mut conn_count, vec![2, 3], vec![5]);
        assert_rate_msg(&rx2, &mut qps_count, &mut conn_count, vec![2, 3], vec![5]);
        assert_eq!(qps_count, 5);
        assert_eq!(conn_count, 10);

        let _ = send_rate_message_to_secondaries(&mut sender_map, 15, 17).await;
        let mut qps_count = 0;
        let mut conn_count = 0;
        assert_rate_msg(
            &rx1,
            &mut qps_count,
            &mut conn_count,
            vec![7, 8],
            vec![8, 9],
        );
        assert_rate_msg(
            &rx2,
            &mut qps_count,
            &mut conn_count,
            vec![7, 8],
            vec![8, 9],
        );
        assert_eq!(qps_count, 15);
        assert_eq!(conn_count, 17);
    }

    fn assert_rate_msg(
        rx2: &StdReceiver<MessageFromPrimary>,
        qps_count: &mut u32,
        conn_count: &mut u32,
        qps: Vec<u32>,
        conn_c: Vec<u32>,
    ) {
        let primary = rx2.recv_timeout(Duration::from_millis(500)).unwrap();
        match primary {
            MessageFromPrimary::Rates(r) => {
                *conn_count += r.connections;
                *qps_count += r.qps;
                assert!(conn_c.contains(&r.connections));
                assert!(qps.contains(&r.qps));
            }
            _ => {
                panic!("unexpected message");
            }
        }
    }

    // async fn assert_rate_msg(
    //     rx1: StdReceiver<MessageFromPrimary>,
    //     mut sender_map: &mut HashMap<String, Sender<MessageFromPrimary>>,
    //     rx2: StdReceiver<MessageFromPrimary>,
    //     qps: u32, conn_count: u32, qps: u32, exp_conn_count: u32,
    // ) {
    //     send_rate_message_to_secondaries(&mut sender_map, 10, 5).await;
    //     let primary = rx1.recv_timeout(Duration::from_millis(500)).unwrap();
    //     let mut conn_count = 0;
    //     match primary {
    //         MessageFromPrimary::Rates(r) => {
    //             conn_count += r.connections;
    //             assert!(r.connections == 3 || r.connections == 2);
    //             assert_eq!(r.qps, 5);
    //         }
    //         _ => {
    //             panic!("unexpected message");
    //         }
    //     }
    //     let primary = rx2.recv_timeout(Duration::from_millis(500)).unwrap();
    //     match primary {
    //         MessageFromPrimary::Rates(r) => {
    //             conn_count += r.connections;
    //             assert!(r.connections == 3 || r.connections == 2);
    //             assert_eq!(r.qps, 5);
    //         }
    //         _ => {
    //             panic!("unexpected message");
    //         }
    //     }
    //     assert_eq!(conn_count, 5);
    // }

    #[tokio::test]
    async fn test_get_sender_for_secondary() {
        init();
        let (tx, _rx) = channel();
        let (listener, port) = start_tcp_listener_random_port().await;
        let handle = tokio::spawn(start_server_with_listener(listener, tx));
        //wait a bit to give listener time to start
        sleep(Duration::from_millis(50)).await;
        let node = cluster_node(3030, port as u32);
        let sender = get_sender_for_secondary(&node).await;
        assert!(sender.is_some());
        handle.abort();
    }

    pub struct TestDiscoverService {
        instances: Vec<ServiceInstance>,
    }

    #[async_trait]
    impl DiscoveryService for TestDiscoverService {
        /// Return list of Kubernetes endpoints as `ServiceInstance`s
        async fn discover_instances(&self) -> Result<Vec<ServiceInstance>, Box<dyn Error>> {
            Ok(self.instances.clone())
        }
    }

    // #[allow(dead_code)]
    // fn get_discovery_service() -> TestDiscoverService {
    //     let mut instances = vec![];
    //     let instance = ServiceInstance::new(
    //         Some(Uuid::new_v4().to_string()),
    //         Some(String::from_str("test").unwrap()),
    //         Some(String::from_str("127.0.0.1").unwrap()),
    //         Some(3030),
    //         false,
    //         Some("http://127.0.0.1:3030".to_string()),
    //         std::collections::HashMap::new(),
    //         Some(String::from_str("HTTP").unwrap()),
    //     );
    //
    //     instances.push(instance);
    //     let instance = ServiceInstance::new(
    //         Some(Uuid::new_v4().to_string()),
    //         Some(String::from_str("test").unwrap()),
    //         Some(String::from_str("127.0.0.1").unwrap()),
    //         Some(3031),
    //         false,
    //         Some("http://127.0.0.1:3031".to_string()),
    //         std::collections::HashMap::new(),
    //         Some(String::from_str("HTTP").unwrap()),
    //     );
    //     instances.push(instance);
    //     let instance = ServiceInstance::new(
    //         Some(Uuid::new_v4().to_string()),
    //         Some(String::from_str("test").unwrap()),
    //         Some(String::from_str("127.0.0.1").unwrap()),
    //         Some(3032),
    //         false,
    //         Some("http://127.0.0.1:3032".to_string()),
    //         std::collections::HashMap::new(),
    //         Some(String::from_str("HTTP").unwrap()),
    //     );
    //     instances.push(instance);
    //
    //     let instance = ServiceInstance::new(
    //         Some(Uuid::new_v4().to_string()),
    //         Some(String::from_str("test").unwrap()),
    //         Some(String::from_str("127.0.0.1").unwrap()),
    //         Some(3033),
    //         false,
    //         Some("http://127.0.0.1:3033".to_string()),
    //         std::collections::HashMap::new(),
    //         Some(String::from_str("HTTP").unwrap()),
    //     );
    //     instances.push(instance);
    //     TestDiscoverService { instances }
    // }
}
