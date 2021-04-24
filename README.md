# Overload
A load testing tool written in Rust

## Usage
Overload support two modes - cluster & standalone

### Cluster
Currently, only supported on Kubernetes.

To run in cluster mode, the `cluster` feature needs to be enabled during the build.
```shell
cargo build --release --features cluster
```
Sample Dockerfile and deployment configurations are provided to build the docker image and deploy it on
Kubernetes

The application will handle clustering related stuff like consensus/leader election, forwarding API call
by itself. Scaling up will add newly created nodes to the cluster or scaling down will remove nodes
automatically.

### Standalone
Build and run a single instance of the application.
```shell
cargo run
```

## APIs

### Run a test
```http request
POST /test HTTP/1.1
Host: localhost:3030
Content-Type: application/json

{
    "duration": 120,
    "req": [
        {
            "method": "GET",
            "url": "http://httpbin.org/",
        }
    ],
    "qps": {
        "Linear": {
            "a": 0.5,
            "b": 1,
            "max":120
        }
    }
}
```
This will run the test for 120 seconds with a linear increase in request per seconds(RPS).

| field | Description | data type
| --- | ----------- | ---------
| duration | Test duration | uint32
| req | Array of HttpReq | [HttpReq](#httpreq)
| qps | RPS specification | [QPSSpec](#qpsspec)

#### Response
| field | Description | data type
| --- | ----------- | ---------
| job_id | Test ID | UUID
| status | status of the test | [JobStatus](#jobstatus)

```json
{
    "job_id": "1b826ded-34d5-434c-b983-a1ba46ffff82",
    "status": "Starting"
}
```

### HttpReq

| field | Description | data type
| --- | ----------- | ---------
| method | HTTP method | Enum(POST,GET)
| url | valid url | string
| body | Body for POST | string

### QPSSpec
Currently, supports the following specifications

#### ConstantQPS
| field | Description | data type
| --- | ----------- | ---------
| qps | QPS to maintain for the `duration` | uint32

#### Linear
Increase QPS linearly; QPS for any time is calculated using eq. qps = ax + b, where x being the
n-th second.

| field | Description | data type
| --- | ----------- | ---------
| a | Slope of the line | float
| b | Intercept | uint32
| max | max QPS | uint32

##### Example
```json
{
  "qps": {
    "Linear": {
      "a": 2,
      "b": 1,
      "max":12
    }
  }
}
```
If a test runs for 10 seconds, the generated RPS will be [1, 5, 7, 9, 11, 12, 12, 12, 12, 12]

#### ArrayQPS
Specify RPS directly

| field | Description | data type
| --- | ----------- | ---------
| qps | Array of qps | [uint32]

##### Example
```json
{
  "qps": {
    "ArrayQPS": {
      "qps": [1, 4, 6, 10, 20, 10]
    }
  }
}
```
If a test runs for 10 seconds, generated RPS will be 1 on the first second, 4 on second seconds, 
6 on third seconds and 10, 20, 10 on 4, 5, 6th second. It'll restart from 0 position once reaches 
the end of the array,
so on the 7th seconds, QPS will go down back to 1 and 4, 6, 10 on 8, 9, 10th seconds, respectively.

### JobStatus
Enum stating the current status of the test

| value | Description
| --- | -----------
| Starting | Job starting or submitted to the queue |
| InProgress | Job is running |
| Stopped | Job stopped by User |
| Completed | Job Done |
| Failed | Some error happened, Couldn't finish executing the job |
| Error(ErrorCode) | Other error |
