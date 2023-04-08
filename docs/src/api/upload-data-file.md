# `POST /test/requests-bin`

Currently, supports CSV file only. Internally, application converts the uploaded file sqlite database.

Curl sample

```shell
curl --location --request POST '{overload_host}:3030/test/requests-bin' \
--header 'Content-Type: text/csv' \
--data-binary '@/path/to/requests.csv'
```

### CSV format

```csv
"url","method","body","headers"
"http://httpbin.org/anything/11","GET","","{}"
"http://httpbin.org/anything/13","GET","","{}"
"http://httpbin.org/anything","POST","{\"some\":\"random data\",\"second-key\":\"more data\"}","{\"Authorization\":\"Bearer 123\"}"
"http://httpbin.org/bearer","GET","","{\"Authorization\":\"Bearer 123\"}"
```
Note that header is required.

### Response

API returns valid count, i.e. count of requests that has been parsed successfully and a file ID. File ID will be
required to for testing.

| field       | Description                      | data type |
|-------------|----------------------------------|-----------|
| valid_count | number of valid requests in file | uint32    |
| file        | ID of the file                   | UUID      |
