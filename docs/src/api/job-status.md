## `GET /test/status`

Returns status of all jobs.

**Limitations -**
* Keep status only for 10 minutes after test ends, will be cleaned up after that

### Query Params

| field  | Description       | data type |
|--------|-------------------|-----------|
| offset | start of the page | uint32    |
| limit  | size of the page  | uint32    |

### Response

| field  | Description        | data type                           |
|--------|--------------------|-------------------------------------|
| job_id | Test identifier    | UUID                                |
| status | status of the test | [JobStatus](../types/job-status.md) |


### Example

```shell
curl --location --request GET '{overload_host}:3030/test/status/'
```

```json
{
  "60de342e-b18c-4837-80d2-a2c71c1985f8": "Completed"
}
```
