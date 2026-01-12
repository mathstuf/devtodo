// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use chrono::{self, Utc};
use graphql_client::GraphQLQuery;
use log::{log, trace, Level};

type DateTime = chrono::DateTime<Utc>;
#[allow(clippy::upper_case_acronyms)]
type URI = String;

macro_rules! gql_query_base {
    ($name:ident) => {
        #[derive(GraphQLQuery)]
        #[graphql(
            schema_path = "src/account/github/graphql/schema.graphql",
            query_path = "src/account/github/graphql/query.graphql",
            deprecated = "warn",
            variables_derives = "Debug, Clone",
            response_derives = "Debug, Clone"
        )]
        pub struct $name;
    };
}

macro_rules! gql_query {
    ($name:ident, $query_name:expr) => {
        gql_query_base!($name);

        impl $name {
            pub(crate) fn name() -> &'static str {
                $query_name
            }
        }
    };
}

gql_query!(ViewerIssues, "ViewerIssues");
gql_query!(ViewerPullRequests, "ViewerPullRequests");

#[derive(Debug, Clone, Copy)]
pub(crate) struct RateLimitInfo {
    pub cost: i64,
    pub limit: i64,
    pub remaining: i64,
    pub reset_at: DateTime,
}

impl RateLimitInfo {
    pub(crate) fn inspect(&self, name: &str) {
        let (level, msg) = match self.remaining {
            0 => {
                (
                    Level::Error,
                    format!(
                        "rate limit has been hit: {} used (resets at {})",
                        self.limit, self.reset_at,
                    ),
                )
            },
            r if r <= 100 => {
                (
                    Level::Warn,
                    format!(
                        "rate limit is nearing: {} / {} left (resets at {})",
                        r, self.limit, self.reset_at,
                    ),
                )
            },
            r if r <= 1000 => {
                (
                    Level::Info,
                    format!(
                        "rate limit is approaching: {} / {} left (resets at {})",
                        r, self.limit, self.reset_at,
                    ),
                )
            },
            r => {
                (
                    Level::Debug,
                    format!(
                        "rate limit is OK: {} / {} left (resets at {})",
                        r, self.limit, self.reset_at,
                    ),
                )
            },
        };

        log!(level, "{name}: {msg}");
        trace!("rate limit cost: {} / {}", self.cost, self.limit);
    }
}

macro_rules! impl_into_rate_limit_info {
    ($type:path) => {
        impl From<$type> for RateLimitInfo {
            fn from(info: $type) -> Self {
                Self {
                    cost: info.cost,
                    limit: info.limit,
                    remaining: info.remaining,
                    reset_at: info.reset_at,
                }
            }
        }
    };
}

impl_into_rate_limit_info!(viewer_issues::RateLimitInfoRateLimit);
impl_into_rate_limit_info!(viewer_pull_requests::RateLimitInfoRateLimit);
