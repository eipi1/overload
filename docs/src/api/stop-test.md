# `GET /test/stop/{test_id}`
Stop a test by id

## Params

| field   | Description                      | data type |
|---------|----------------------------------|-----------|
| test_id | id of the test job to be stopped | string    |

## Example
```shell
curl --location --request GET '{overload_host}:3030/test/stop/multi-req-5d1b90e8-a1e0-4d00-b192-e80b8f36d038'
```
