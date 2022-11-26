## Step/Staircase QPS

Increase or decrease connection pool size in steps.

{{#include ../types/step-rate.md:3:5}}

### Step
{{#include ../types/step-rate.md:9:13}}

### Example
{{#include ../types/step-rate.md:17:46}}

If the test run for 15 seconds, from 0 to 5th seconds, 1 connection will be used, from 6th to 8th seconds, 
it'll be 4, from 9 to 10th and 11th to 15th seconds, 7 connections will be used.
