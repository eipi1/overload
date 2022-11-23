# Configurations

The application supports following environment variables - 

### RUST_LOG
Configure logging level, default is `info`.

You can either configure logging levels for all the components or set different level for different
components. For example - 
* To enable trace logging - `RUST_LOG=trace`
* To enable debug log for executor(handles test request), but info for other - `RUST_LOG=info,cluster_executor=debug`

### DATA_DIR
Path to store uploaded CSV files, default is `/tmp`.

### K8S_ENDPOINT_NAME
Name of the [endpoints](https://kubernetes.io/docs/reference/generated/kubernetes-api/v1.19/#read-endpoints-v1-core), default is `overload`.

### K8S_NAMESPACE_NAME
The kubernetes namespace the application is deployed on, default is `default`.

### CLUSTER_UPDATE_INTERVAL
Interval between discovery service calls in seconds, default is `10`.

Setting it too low can cause frequent call to k8s API, and if set too high, it may take long time to
discover new pods.

### CLUSTER_ELECTION_TIMEOUT
Approximate interval in seconds between attempts to elect new leader node. Application will use
a randomly chosen value between CLUSTER_ELECTION_TIMEOUT and CLUSTER_ELECTION_TIMEOUT*2.

Default is `30`

### CLUSTER_MAX_NODE
Maximum number of allowed node in a cluster. Minimum is `4`, default `20`.

### CLUSTER_MIN_NODE
Minimum number of nodes required to create a cluster. Minimum is `4`, default `4`.


