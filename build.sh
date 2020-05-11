#!/bin/bash
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
echo "$DIR"
cargo build
if ! $(cargo build); then
  echo "Build failed."
  exit
fi
cd "$DIR"
TAG="10.1.64.1:5000/overload:latest"
docker build -t $TAG .
docker push $TAG
kubectl delete deployments overload
kubectl create deployment overload --image=10.1.64.1:5000/overload
kubectl get all