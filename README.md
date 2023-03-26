# overload
[![Github Pages](https://github.com/eipi1/overload/actions/workflows/github-pages.yml/badge.svg)](https://eipi1.github.io/overload/)
![Rust](https://github.com/eipi1/overload/actions/workflows/rust.yml/badge.svg)
[![MIT licensed][mit-badge]][mit-url]
[![Github][github-badge]][github-rep]


Overload is a distributed, scalable performance testing application. It mainly focuses on ease-of-use.
* Distributed - users don't need to handle complex cluster management - the application creates and manages the cluster by itself once deployed.
* Control - better control over QPS, connection pool, request data
* Assertion - easy assertion with lua script

For details usage and reference please check https://eipi1.github.io/overload/

[mit-badge]: https://img.shields.io/badge/license-MIT-blue.svg

[mit-url]: https://github.com/tokio-rs/tokio/blob/master/LICENSE

[github-badge]: https://img.shields.io/badge/github-eipi1/overload-brightgreen

[github-rep]: https://github.com/eipi1/overload

## Getting started
### Docker
```shell
docker run -p 3030:3030 ghcr.io/eipi1/overload:latest-standalone-snapshot
```

### Kubernetes
```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  labels:
    app: overload
  name: overload
  namespace: default
spec:
  replicas: 1
  selector:
    matchLabels:
      app: overload
  template:
    metadata:
      labels:
        app: overload
    spec:
      containers:
        - image: ghcr.io/eipi1/overload:latest-cluster-snapshot
          imagePullPolicy: "IfNotPresent"
          name: overload
          ports:
            - containerPort: 3030
            - containerPort: 3031
          env:
            - name: DATA_DIR
              value: "/tmp"
            - name: RUST_BACKTRACE
              value: "1"
            - name: K8S_ENDPOINT_NAME
              value: "overload"
            - name: K8S_NAMESPACE_NAME
              value: "default"
            - name: RUST_LOG
              value: info
---
apiVersion: v1
kind: Service
metadata:
  name: overload
spec:
  #  type: LoadBalancer
  selector:
    app: overload
  ports:
    - protocol: TCP
      port: 3030
      targetPort: 3030
      name: http-endpoint
    - protocol: TCP
      port: 3031
      targetPort: 3031
      name: tcp-remoc
```

### Start Test
The following request will send two `GET` request per second(`"countPerSec": 2`) to `httpbin.org/get` for 120
seconds(`"duration": 60`).
```shell
curl --location --request POST '{overload-host}:3030/test' \
--header 'Content-Type: application/json' \
--data-raw '<json_request_body>'
```

Sample JSON request body -

```json
{
  "duration": 120,
  "name": "demo-test",
  "qps": {
    "ConstantRate": {
      "countPerSec": 1
    }
  },
  "req": {
    "RequestList": {
      "data": [
        {
          "method": "GET",
          "url": "/get"
        }
      ]
    }
  },
  "target": {
    "host": "httpbin.org",
    "port": 80,
    "protocol": "HTTP"
  }
}
```


It'll respond with a job identifier and status.
```json
{
  "job_id": "demo-test-d2ae5ff0-7bf4-4daf-8784-83b642d7dd6b",
  "status": "Starting"
}
```
We'll need the `job_id` if we want to stop the test later.

### Get job status
```shell
curl --location --request GET '{overload-host}:3030/test/status/'
```
### Stop a job
```shell
curl --location --request GET '{overload-host}:3030/test/stop/demo-test-d2ae5ff0-7bf4-4daf-8784-83b642d7dd6b'
```

## Monitoring
Overload exposes details data on QPS, assertion result, etc through prometheus. For details please check [here](https://eipi1.github.io/overload/monitoring.html)

License: MIT
