# `POST /request-file/{sqlite|csv}`

Currently, supports CSV & SQLite files. Internally, application converts the uploaded file sqlite database.

Curl sample

```shell
curl --location --request POST '{overload_host}:3030/request-file/sqlite' \
--data-binary '@/path/to/requests.sqlite'
```

### CSV format (Deprecated, use sqlite instead)
It's not recommended to use CSV format for large file as Overload have to convert CSV to Sqlite. The conversion
process may take time.

The CSV should have header included.

```csv
"method","url","body","headers"
"POST","/sample/path/0","{\"sample\":\"json body\",\"host\":\"127.0.0.1\",\"port\":2080,\"protocol\":\"HTTP\"}","{\"Connection\":\"keep-alive\"}"
"POST","/sample/path/1","{\"sample\":\"json body\",\"host\":\"127.0.0.1\",\"port\":2080,\"protocol\":\"HTTP\"}","{\"Connection\":\"keep-alive\"}"
"POST","/sample/path/2","{\"sample\":\"json body\",\"host\":\"127.0.0.1\",\"port\":2080,\"protocol\":\"HTTP\"}","{\"Connection\":\"keep-alive\"}"
"POST","/sample/path/3","{\"sample\":\"json body\",\"host\":\"127.0.0.1\",\"port\":2080,\"protocol\":\"HTTP\"}","{\"Connection\":\"keep-alive\"}"
"POST","/sample/path/4","{\"sample\":\"json body\",\"host\":\"127.0.0.1\",\"port\":2080,\"protocol\":\"HTTP\"}","{\"Connection\":\"keep-alive\"}"
"POST","/sample/path/5","{\"sample\":\"json body\",\"host\":\"127.0.0.1\",\"port\":2080,\"protocol\":\"HTTP\"}","{\"Connection\":\"keep-alive\"}"
"GET","/sample/path/6","","{\"Connection\":\"keep-alive\"}"
"GET","/sample/path/7","","{\"Connection\":\"keep-alive\"}"
```

### Sqlite format
The recommended format for large request file.

One of the many ways to convert CSV to Sqlite is follows -

```shell
sqlite3 sample-request.sqlite "VACUUM;"
csvsql -p '\' --db sqlite:///sample-request.sqlite --table http_req --overwrite --insert ./sample-request.csv
```
Generated sqlite of the sample CSV above -
```shell
$ sqlite3 sample-request.sqlite 
SQLite version 3.37.2 2022-01-06 13:25:41
Enter ".help" for usage hints.
sqlite> select * from http_req;
POST|/sample/path/0|{"sample":"json body","host":"127.0.0.1","port":2080,"protocol":"HTTP"}|{"Connection":"keep-alive"}
POST|/sample/path/1|{"sample":"json body","host":"127.0.0.1","port":2080,"protocol":"HTTP"}|{"Connection":"keep-alive"}
POST|/sample/path/2|{"sample":"json body","host":"127.0.0.1","port":2080,"protocol":"HTTP"}|{"Connection":"keep-alive"}
POST|/sample/path/3|{"sample":"json body","host":"127.0.0.1","port":2080,"protocol":"HTTP"}|{"Connection":"keep-alive"}
POST|/sample/path/4|{"sample":"json body","host":"127.0.0.1","port":2080,"protocol":"HTTP"}|{"Connection":"keep-alive"}
POST|/sample/path/5|{"sample":"json body","host":"127.0.0.1","port":2080,"protocol":"HTTP"}|{"Connection":"keep-alive"}
GET|/sample/path/6||{"Connection":"keep-alive"}
GET|/sample/path/7||{"Connection":"keep-alive"}
sqlite> 
```

To upload using curl -
```shell
curl --location '{overload_host}:3030/request-file/sqlite' \
--header 'Content-Type: application/octet-stream' \
--data-binary '@/path/to/test-requests-post.sqlite'
```

### Support for Gzip (version: SNAPSHOT)
To allow larger and faster upload, Overload also supports gzipped sqlite file, but requires "Content-Encoding: gzip" header in the request. The uploaded
sqlite file will be unzipped in the server.

To upload using curl
```shell
cat /path/to/test-requests-post.sqlite | gzip | \
curl -vi --location '{overload_host}:3030/request-file/sqlite' \
--header "Content-Encoding: gzip" \
--header 'Content-Type: application/octet-stream' \
--data-binary @-
```

### Response

API returns valid count, i.e. count of requests that has been parsed successfully and a file ID. File ID will be
required to for testing.

| field       | Description                      | data type |
|-------------|----------------------------------|-----------|
| valid_count | number of valid requests in file | uint32    |
| file        | ID of the file                   | UUID      |
