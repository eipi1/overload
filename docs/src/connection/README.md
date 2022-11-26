# Connection Pool Specification

`concurrentConnection` field is used to specify the number of connections to be used during the test.

By default, connection pool is elastic - it create as many connection as needed.

Connections are maintained in a circular queue, i.e. once a connection is used, it goes back to the
end of the queue. This way, we can ensure that all connection in the pool are getting used.
If a connection is closed by remote target, a new connection will be created.

Currently supports following configurations -  
* [ConstantRate](constant.md)
* [Linear](linear.md)
* [Steps/Staircase](steps-qps.md)
