# Deploy on Kubernetes

Cluster mode allows the application run in primary/secondary mode. Currently, only supported on Kubernetes.
**Application requires at least 4 pods to work in cluster mode.**

Running on Kubernetes requires minimum four pods and [cluster images](https://github.com/eipi1/overload/pkgs/container/overload).
Cluster images are tagged as *cluster-{version}*.

Addition to that the application requires RBAC authorization to "get", "list" [endpoints](https://kubernetes.io/docs/reference/generated/kubernetes-api/v1.22/#endpoints-v1-core).

Sample deployment configuration -
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
        - image: ghcr.io/eipi1/overload:cluster-latest-snapshot
          imagePullPolicy: "Always"
          name: overload
          ports:
            - containerPort: 3030
            - containerPort: 3031
          env:
            - name: DATA_DIR
              value: "/tmp"
            - name: K8S_ENDPOINT_NAME
              value: "overload"
            - name: K8S_NAMESPACE_NAME
              value: "default"
            - name: RUST_LOG
              value: info
          resources:
            requests:
              memory: "64Mi"
              cpu: "250m"
            limits:
              memory: "1024Mi"
              cpu: "2000m"
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
      name: tcp-remoc #default, pass to env CLUSTER_COM_PORT_NAME if changed
```