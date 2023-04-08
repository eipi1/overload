use crate::request_providers::RequestProvider;
use crate::{log_error, MessageFromPrimary};
use log::error;
use overload_http::{Request, RequestSpecEnum};
use remoc::rch::base::Sender;
use std::cmp::min;
use std::collections::{HashMap, VecDeque};

pub(crate) async fn process_and_send_request(
    request: Request,
    senders: &mut HashMap<String, Sender<MessageFromPrimary>>,
) -> Result<(), Vec<&String>> {
    let request_ = request.clone();
    let mut error_instances = vec![];
    if let RequestSpecEnum::SplitRequestFile(mut req) = request.req {
        if let Err(e) = req.get_n(1).await {
            error!(
                "[process_and_send_request] - could not get the sample count from the file - {:?}, error: {}",
                &req, e
            );
        } else {
            let req_data_size = req.size_hint();
            let mut splits = split_ranges(req_data_size, senders.len());
            for (id, sender) in senders {
                let mut request = request_.clone();
                request.req = RequestSpecEnum::SplitRequestFile(
                    req.clone_with_new_range(splits.pop_front().unwrap()),
                );
                let result = sender.send(MessageFromPrimary::Request(request)).await;
                log_error!(result);
                error_instances.push(id);
            }
        }
    }
    if error_instances.is_empty() {
        Ok(())
    } else {
        Err(error_instances)
    }
}

fn split_ranges(size: usize, split_size: usize) -> VecDeque<(usize, usize)> {
    let block_size = size / split_size;
    let remainder = size % split_size;
    let mut splits = VecDeque::with_capacity(split_size);
    let mut last_end = 0;
    for _ in 0..remainder {
        let new_end = min(size, last_end + block_size + 1);
        splits.push_back((min(last_end + 1, size), new_end));
        last_end = new_end;
    }
    for _ in remainder..split_size {
        let new_end = min(size, last_end + block_size);
        splits.push_back((min(last_end + 1, size), new_end));
        last_end = new_end;
    }
    splits
}

#[cfg(test)]
mod test {
    use crate::split_request::split_ranges;
    use std::collections::VecDeque;

    #[test]
    fn test_split_ranges() {
        let verify = |splits: VecDeque<(usize, usize)>, end: usize, size: usize| {
            println!("{:?}", splits);
            assert_eq!(splits.len(), size);
            let (_, e) = splits.back().unwrap();
            assert_eq!(*e, end);
            let (s, last_e) = splits.get(0).unwrap();
            assert!(s <= last_e);
            let mut last_e = *last_e;
            for i in 1..splits.len() {
                let (s, e) = splits.get(i).unwrap();
                assert!(s <= e);
                if end > size * 2 {
                    assert_eq!(s - last_e, 1);
                }
                last_e = *e;
            }
        };
        verify(split_ranges(10, 4), 10, 4);
        verify(split_ranges(10, 3), 10, 3);
        verify(split_ranges(11, 4), 11, 4);
        verify(split_ranges(11, 3), 11, 3);
        verify(split_ranges(1, 4), 1, 4);
        verify(split_ranges(4, 4), 4, 4);
        verify(split_ranges(7, 4), 7, 4);
        verify(split_ranges(8, 4), 8, 4);
        verify(split_ranges(9, 4), 9, 4);
    }
}
