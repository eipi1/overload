# `MultiRequest`

| Field    | Required | Default | Data type               | Description                                                         |
|----------|----------|---------|-------------------------|---------------------------------------------------------------------|
| name     | ❎        | UUID    | String                  | Test name, application will append UUID to ensure unique identifier |
| requests | ✅        |         | [Request](./request.md) | Collection of Request to execute                                    |
