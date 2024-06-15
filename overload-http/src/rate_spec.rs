use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ConstantRate {
    pub count_per_sec: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ArraySpec {
    pub count_per_sec: Vec<u32>,
}

impl ArraySpec {
    pub fn new(count_per_sec: Vec<u32>) -> Self {
        Self { count_per_sec }
    }
}

/// Used only for concurrent connections, creates a pool of maximum size specified
#[derive(Debug, Serialize, Deserialize)]
pub struct Bounded {
    pub max: u32,
}

impl Default for Bounded {
    fn default() -> Self {
        Bounded { max: 500 }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Elastic {
    pub(crate) max: u32,
}

impl Default for Elastic {
    fn default() -> Self {
        Self {
            max: u16::MAX as u32,
        }
    }
}

/// Increase QPS linearly as per equation
/// `y=ceil(ax+b)` until hit the max cap
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Linear {
    pub a: f32,
    pub b: u32,
    pub max: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(try_from = "StepShadowType")]
pub struct Step {
    pub start: u32,
    pub end: u32,
    pub rate: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(try_from = "StepsShadowType")]
pub struct Steps {
    #[serde(skip)]
    pub current_step: usize,
    pub steps: Vec<Step>,
}

#[derive(Deserialize)]
pub struct StepShadowType {
    start: u32,
    end: u32,
    rate: u32,
}

impl TryFrom<StepShadowType> for Step {
    type Error = anyhow::Error;

    fn try_from(value: StepShadowType) -> Result<Self, Self::Error> {
        if value.start >= value.end {
            return Err(anyhow::anyhow!(
                "start({}) can not be greater than or equal to end({})",
                value.start,
                value.end
            ));
        }
        Ok(Step {
            start: value.start,
            end: value.end,
            rate: value.rate,
        })
    }
}

#[derive(Deserialize)]
pub struct StepsShadowType {
    steps: Vec<Step>,
}

impl TryFrom<StepsShadowType> for Steps {
    type Error = anyhow::Error;

    fn try_from(value: StepsShadowType) -> Result<Self, Self::Error> {
        let mut value = value;
        if value.steps.is_empty() {
            return Err(anyhow::anyhow!("No steps found."));
        }
        if value.steps.len() == 1 {
            return Err(anyhow::anyhow!(
                "Need more than one step. Use ConstantQPS for single step."
            ));
        }
        value.steps.sort_by(|a, b| a.start.cmp(&b.start));
        //check continuity of steps
        let steps = &value.steps;
        let mut iter = steps.iter();
        let first = iter.next().unwrap();
        if first.start != 0 {
            return Err(anyhow::anyhow!(
                "Steps should start from 0, but starts from {} instead.",
                first.start
            ));
        }

        let mut prev = first.end;
        for current in iter {
            if prev + 1 != current.start {
                return Err(anyhow::anyhow!("Steps are not continuous. An step ends at {} but next step should start at {}, but starts at {}",
                 prev, prev+1, current.start));
            } else {
                prev = current.end;
            }
        }
        Ok(Steps {
            current_step: 0,
            steps: value.steps,
        })
    }
}
