use log::warn;
use overload_http::{ArraySpec, Bounded, ConstantRate, Elastic, Linear, Steps};
use std::cmp::min;

pub trait RateScheme {
    fn next(&mut self, nth: u32, last_qps: Option<u32>) -> u32;
}

impl RateScheme for ConstantRate {
    #[inline]
    fn next(&mut self, _nth: u32, _last_qps: Option<u32>) -> u32 {
        self.count_per_sec
    }
}

impl RateScheme for ArraySpec {
    #[inline]
    fn next(&mut self, nth: u32, _last_qps: Option<u32>) -> u32 {
        let len = self.count_per_sec.len();
        if len != 0 {
            let idx = nth as usize % len;
            let val = self.count_per_sec.get(idx).unwrap();
            *val
        } else {
            0
        }
    }
}

impl RateScheme for Bounded {
    fn next(&mut self, _nth: u32, _last_value: Option<u32>) -> u32 {
        self.max
    }
}

impl RateScheme for Linear {
    fn next(&mut self, nth: u32, _last_qps: Option<u32>) -> u32 {
        min((self.a * nth as f32).ceil() as u32 + self.b, self.max)
    }
}

impl RateScheme for Steps {
    fn next(&mut self, nth: u32, _last_qps: Option<u32>) -> u32 {
        let x = if let Some(step) = self.steps.get(self.current_step) {
            if step.start <= nth && step.end >= nth {
                (step.rate, false)
            } else {
                //not found in current_step, check next
                self.steps.get(self.current_step + 1).map_or_else(
                    || {
                        (
                            //not found in current_step+1, use last step as fallback
                            self.steps.last().map_or(0, |s| s.rate),
                            // (self.steps.len() - 1) - self.current_step,
                            false,
                        )
                    },
                    |s| (s.rate, true),
                )
            }
        } else {
            warn!("Invalid current step");
            (0, false)
        };
        if x.1 {
            self.current_step += 1;
        }
        x.0
    }
}

impl RateScheme for Elastic {
    fn next(&mut self, _nth: u32, _last_qps: Option<u32>) -> u32 {
        0
    }
}
