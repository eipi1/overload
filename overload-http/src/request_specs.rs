use crate::HttpReq;
// use anyhow::Result as AnyResult;
use datagen::DataSchema;
use http::Method;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use smol_str::SmolStr;
use sqlx::SqliteConnection;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RequestList {
    pub data: Vec<HttpReq>,
}

impl From<Vec<HttpReq>> for RequestList {
    fn from(data: Vec<HttpReq>) -> Self {
        RequestList { data }
    }
}

/// Test request with file data
#[derive(Debug, Serialize, Deserialize)]
pub struct RequestFile {
    pub file_name: String,
    #[serde(skip)]
    pub inner: Option<SqliteConnection>,
    #[serde(skip)]
    #[serde(default = "default_request_file_size")]
    pub size: usize,
}

impl RequestFile {
    pub fn new(file_name: String) -> RequestFile {
        RequestFile {
            file_name,
            inner: None,
            size: default_request_file_size(),
        }
    }
}

impl Clone for RequestFile {
    fn clone(&self) -> Self {
        RequestFile::new(self.file_name.clone())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UrlParam {
    name: SmolStr,
    pub start: usize,
    end: usize,
}

impl Eq for UrlParam {}

impl PartialEq<Self> for UrlParam {
    fn eq(&self, other: &Self) -> bool {
        self.start.eq(&other.start)
    }
}

impl PartialOrd<Self> for UrlParam {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.start.partial_cmp(&other.start)
    }
}

impl Ord for UrlParam {
    fn cmp(&self, other: &Self) -> Ordering {
        self.start.cmp(&other.start)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::type_complexity)]
pub struct RandomDataRequest {
    #[serde(skip)]
    #[serde(default)]
    pub init: bool,
    #[serde(with = "http_serde::method")]
    pub method: Method,
    //todo as a http::Uri
    pub url: String,
    #[serde(skip)]
    pub url_param_pos: Option<BinaryHeap<UrlParam>>,
    #[serde(default = "HashMap::new")]
    pub headers: HashMap<String, String>,
    pub body_schema: Option<DataSchema>,
    pub uri_param_schema: Option<DataSchema>,
}

impl RandomDataRequest {
    pub fn find_param_positions(url: &str) -> BinaryHeap<UrlParam> {
        let re = Regex::new("\\{[a-zA-Z0-9_-]+}").unwrap();
        // let url_str = url;
        let matches = re.find_iter(url);
        let mut match_indices = BinaryHeap::new();
        for m in matches {
            match_indices.push(UrlParam {
                name: SmolStr::new(&url[m.start() + 1..m.end() - 1]),
                start: m.start(),
                end: m.end(),
            });
        }
        match_indices
    }

    pub fn substitute_param_with_data(
        url: &mut String,
        positions: &BinaryHeap<UrlParam>,
        data: Value,
    ) {
        for url_param in positions.iter() {
            if let Some(p_val) = data.get(url_param.name.as_str()) {
                let val: String = if p_val.is_string() {
                    String::from(p_val.as_str().unwrap())
                } else {
                    p_val
                        .as_i64()
                        .map_or(String::from("unknown"), |t| t.to_string())
                };
                url.replace_range(url_param.start..url_param.end, &val);
            }
        }
    }
}

fn default_request_file_size() -> usize {
    999
}
