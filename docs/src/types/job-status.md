# JobStatus

Enum stating the current status of the test

| value            | Description                                            |
|------------------|--------------------------------------------------------|
| Starting         | Job starting or submitted to the queue                 |
| InProgress       | Job is running                                         |
| Stopped          | Job stopped by User                                    |
| Completed        | Job Done                                               |
| Failed           | Some error happened, Couldn't finish executing the job |
| Error(ErrorCode) | Other errors                                           |
