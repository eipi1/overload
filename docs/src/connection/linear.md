### Linear

Increase connection pool size linearly; rate for any time is calculated using equation `connection_count = ax + b`,
where `x` being time(in seconds) passed since the start of the test.

{{#include ../types/linear-rate.md:3:7}}

#### Example

{{#include ../types/linear-rate.md:11:26}}

If a test runs for 10 seconds, the number of connections will be \[1, 5, 7, 9, 11, 12, 12, 12, 12, 12\]