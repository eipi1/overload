//! Define common types uses by various components of overload

use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;

/// Specify how requests should be generated at any specific moment("second")
///
/// In conjunction with these options along with QPS & concurrent connection configuration, the
/// generator can be configured either to [open or closed system](https://kilthub.cmu.edu/articles/journal_contribution/Open_Versus_Closed_A_Cautionary_Tale/6608078)
///
/// ### Batch
/// Sends the requests in batches at certain intervals. The generator keeps track of available time
/// to send all the required batches. If it fails to send a batch or any request in the batch,
/// intervals might be reduced to accommodate remaining requests in the available time.
///
/// #### Example
/// Consider the generator needs to produce 100 requests in a certain *second*, and the batch size
/// is 10.
///
/// In this case, the *second* will be sliced into ten 100ms periods. At the beginning of each slice,
/// 10 requests will be sent to the target. If there is enough connection available in the pool, no
/// new connection will be created, otherwise new connections will be attempted. Note that the
/// connection creation is controlled by concurrent connection configuration.
///
/// If generator failed to send the second batch due to connection availability, slices will be
/// recalculated. Now 9 batches needs to be sent in ~800ms. the slice duration will be updated to
/// (100/9)=~88ms.
///
/// Time mentioned here is simplified and actual slice duration can be shorter. The generator
/// reserves 10%(~100ms) for itself and may consume more than that.
///
/// ### Immediate
/// Tries to send all the requests at the beginning of the *second*. Crates as much as connection as
/// required as long as it's allowed by concurrent connection configuration. If enough connections
/// are not available, generator waits for next a(or more) connection to be available, and sends the
/// request.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum LoadGenerationMode {
    Batch { batch_size: NonZeroUsize },
    Immediate,
}
