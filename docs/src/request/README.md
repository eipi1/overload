# Request Specification
Application supports following request sources -
* [RequestList](list.md) - an array of requests, generally useful for small set of request data
* [RandomDataRequest](random.md) - generate random request based on JSON Schema, useful for large amount of request
* [RequestFile](file.md) - get request from file. Use if [RandomDataRequest](random.md) doesn't satisfy the requirement
* [SplitRequestFile](split-file.md) - Split the request file among secondary nodes
* [JsonTemplate](template.md) - Json request specification with template support