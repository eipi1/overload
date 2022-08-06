use prometheus::{HistogramOpts, HistogramVec, IntCounter, IntCounterVec, Opts, Registry, Gauge};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// macro_rules! common_create_metrics {
//     ($metrics:expr, $desc:literal) => {
//         $metrics.with_description($desc).init();
//     };
// }
//
// macro_rules! init_metrics {
//     ($metric: ident, $name:ident, $str_name: literal, Counter<u64>) => {
//         let $name = common_create_metrics!($metric.u64_counter($str_name), "");
//     };
//     ($metric: ident, $name:ident, $str_name: literal, ValueRecorder<u64>) => {
//         let $name = common_create_metrics!($metric.u64_value_recorder($str_name), "");
//     };
// }

pub const DEFAULT_HISTOGRAM_BUCKET: [f64; 6] = [20f64, 50f64, 100f64, 300f64, 700f64, 1100f64];

#[derive(Default)]
pub struct MetricsFactory {
    registry: Registry,
    metrics: RwLock<HashMap<String, Arc<Metrics>>>,
}

impl MetricsFactory {
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub async fn metrics(&self, job_id: &str) -> Arc<Metrics> {
        self.metrics_with_buckets(Vec::from(DEFAULT_HISTOGRAM_BUCKET), job_id)
            .await
    }

    pub async fn metrics_with_buckets(&self, buckets: Vec<f64>, job_id: &str) -> Arc<Metrics> {
        {
            if let Some(m) = self.metrics.read().await.get(job_id) {
                return m.clone();
            }
        }

        let mut write_guard = self.metrics.write().await;
        //retry again to check if another thread already created metrics
        if let Some(m) = write_guard.get(job_id) {
            return m.clone();
        }

        let opts = HistogramOpts::new("upstream_response_time", "upstream response time");
        let mut opts = opts.const_label("job_id", job_id);
        opts.buckets = buckets;
        let upstream_response_time = HistogramVec::new(opts, &["status"]).unwrap();

        let opts = Opts::new(
            "upstream_request_status_count",
            "upstream request count per status",
        );
        let opts = opts.const_label("job_id", job_id);
        let upstream_request_status_count = IntCounterVec::new(opts, &["status"]).unwrap();

        let opts = Opts::new("upstream_request_count", "request sent to upstream");
        let opts = opts.const_label("job_id", job_id);
        let upstream_request_count = IntCounter::with_opts(opts).unwrap();

        let opts = Opts::new("connection_pool_connections", "total number of connections in the pool");
        let opts = opts.const_label("job_id", job_id);
        let connection_pool_connections = Gauge::with_opts(opts).unwrap();

        let opts = Opts::new("connection_pool_idle_connections", "number of idle connections in the pool");
        let opts = opts.const_label("job_id", job_id);
        let connection_pool_idle_connections = Gauge::with_opts(opts).unwrap();


        self.registry
            .register(Box::new(upstream_response_time.clone()))
            .unwrap();
        self.registry
            .register(Box::new(upstream_request_status_count.clone()))
            .unwrap();
        self.registry
            .register(Box::new(upstream_request_count.clone()))
            .unwrap();
        self.registry
            .register(Box::new(connection_pool_connections.clone()))
            .unwrap();
        self.registry
            .register(Box::new(connection_pool_idle_connections.clone()))
            .unwrap();

        let metrics = Metrics {
            upstream_request_count,
            upstream_request_status_count,
            upstream_response_time,
            connection_pool_connections,
            connection_pool_idle_connections
        };
        let metrics = Arc::new(metrics);
        write_guard.insert(String::from(job_id), metrics.clone());
        metrics
    }
}

pub struct Metrics {
    upstream_request_status_count: IntCounterVec,
    upstream_request_count: IntCounter,
    upstream_response_time: HistogramVec,
    connection_pool_connections: Gauge,
    connection_pool_idle_connections: Gauge
}

impl Metrics {
    pub fn upstream_request_count(&self, increment: u64) {
        self.upstream_request_count.inc_by(increment);
    }

    pub fn upstream_request_status_count(&self, increment: u64, status: &str) {
        self.upstream_request_status_count
            .with_label_values(&[status])
            .inc_by(increment);
    }

    pub fn upstream_response_time(&self, status: &str, elapsed: f64) {
        self.upstream_response_time
            .with_label_values(&[status])
            .observe(elapsed);
    }
    pub fn pool_state(&self, state: (u32, u32)) {
        self.connection_pool_connections.set(state.0.into());
        self.connection_pool_idle_connections.set(state.1.into());
    }
}

pub fn default_histogram_bucket() -> SmallVec<[f64; 6]> {
    smallvec::SmallVec::from(DEFAULT_HISTOGRAM_BUCKET)
}

#[cfg(test)]
mod test {
    use crate::metrics::MetricsFactory;
    use prometheus::{Encoder, TextEncoder};

    #[tokio::test]
    async fn test_metrics_factory() {
        let factory = MetricsFactory::default();
        let m1 = factory.metrics("job_id_1").await;
        m1.upstream_response_time("200", 100f64);
        m1.upstream_request_status_count(5, "200");
        m1.upstream_request_status_count(2, "400");
        m1.upstream_request_count(5);
        m1.upstream_request_count(2);
        let buckets = vec![100f64, 200f64];
        let m2 = factory.metrics_with_buckets(buckets, "job_id_2").await;
        m2.upstream_response_time("200", 200f64);
        m2.upstream_response_time("400", 100f64);
        m2.upstream_request_status_count(2, "200");
        m2.upstream_request_status_count(3, "500");
        m2.upstream_request_count(5);
        let encoder = TextEncoder::new();
        let metrics = factory.registry.gather();
        let mut resp_buffer = vec![];
        let _result = encoder.encode(&metrics, &mut resp_buffer);
        println!("{}", String::from_utf8(resp_buffer).unwrap());
    }

    #[tokio::test]
    async fn test_metrics_factory_same_job() {
        let factory = MetricsFactory::default();
        let m1 = factory.metrics("job_id_1").await;
        m1.upstream_response_time("200", 100f64);
        let buckets = vec![100f64, 200f64];
        let m2 = factory.metrics_with_buckets(buckets, "job_id_1").await;
        m2.upstream_response_time("200", 0.15f64);
        let encoder = TextEncoder::new();
        let metrics = factory.registry.gather();
        let mut resp_buffer = vec![];
        let _result = encoder.encode(&metrics, &mut resp_buffer);
        println!("{}", String::from_utf8(resp_buffer).unwrap());
    }
}
