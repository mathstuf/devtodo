// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::env;
use std::fmt::Debug;
use std::thread;
use std::time::Duration;

use graphql_client::{GraphQLQuery, QueryBody, Response};
use itertools::Itertools;
use log::{info, warn};
use reqwest::blocking::Client;
use reqwest::header::{self, HeaderMap, HeaderValue};
use reqwest::{self, Url};
use serde::Deserialize;
use thiserror::Error;

// The maximum number of times we will retry server errors.
const BACKOFF_LIMIT: usize = 5;
// The number of seconds to start retries at.
const BACKOFF_START: Duration = Duration::from_secs(1);
// How much to scale retry timeouts for a single query.
const BACKOFF_SCALE: u32 = 2;

#[derive(Debug, Error)]
pub enum GithubError {
    #[error("url parse error: {}", source)]
    UrlParse {
        #[from]
        source: url::ParseError,
    },
    #[error("failed to send request to {}: {}", endpoint, source)]
    SendRequest {
        endpoint: Url,
        source: reqwest::Error,
    },
    #[error("github error: {}", response)]
    Github { response: String },
    #[error("deserialize error: {}", source)]
    Deserialize {
        #[from]
        source: serde_json::Error,
    },
    #[error("github service error: {}", status)]
    GithubService { status: reqwest::StatusCode },
    #[error("json response deserialize: {}", source)]
    JsonResponse { source: reqwest::Error },
    #[error("graphql error: [\"{}\"]", message.iter().format("\", \""))]
    GraphQL { message: Vec<graphql_client::Error> },
    #[error("no response from github")]
    NoResponse {},
    #[error("failure even after exponential backoff")]
    GithubBackoff {},
}

impl GithubError {
    fn should_backoff(&self) -> bool {
        if let GithubError::GithubService {
            ..
        } = self
        {
            true
        } else {
            false
        }
    }

    pub fn send_request(endpoint: Url, source: reqwest::Error) -> Self {
        GithubError::SendRequest {
            endpoint,
            source,
        }
    }

    pub fn github(response: String) -> Self {
        GithubError::Github {
            response,
        }
    }

    fn github_service(status: reqwest::StatusCode) -> Self {
        GithubError::GithubService {
            status,
        }
    }

    pub fn json_response(source: reqwest::Error) -> Self {
        GithubError::JsonResponse {
            source,
        }
    }

    fn graphql(message: Vec<graphql_client::Error>) -> Self {
        GithubError::GraphQL {
            message,
        }
    }

    fn no_response() -> Self {
        GithubError::NoResponse {}
    }

    fn github_backoff() -> Self {
        GithubError::GithubBackoff {}
    }
}

pub type GithubResult<T> = Result<T, GithubError>;

// The user agent for all queries.
pub const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), " v", env!("CARGO_PKG_VERSION"));

/// A client for communicating with a Github instance.
#[derive(Clone)]
pub struct Github {
    /// The client used to communicate with Github.
    client: Client,
    /// The endpoint for GraphQL queries.
    gql_endpoint: Url,

    /// The token for the client.
    token: String,
}

impl Github {
    pub fn new<T>(host: &str, token: T) -> GithubResult<Self>
    where
        T: Into<String>,
    {
        let gql_endpoint = Url::parse(&format!("https://{}/graphql", host))?;

        Ok(Github {
            client: Client::new(),
            gql_endpoint,
            token: token.into(),
        })
    }

    /// The authorization header for GraphQL.
    fn auth_header(&self) -> GithubResult<HeaderMap> {
        let mut header_value: HeaderValue = format!("bearer {}", self.token).parse().unwrap();
        header_value.set_sensitive(true);
        Ok([(header::AUTHORIZATION, header_value)]
            .iter()
            .cloned()
            .collect())
    }

    /// Send a GraphQL query.
    fn send_impl<Q>(&self, query: &QueryBody<Q::Variables>) -> GithubResult<Q::ResponseData>
    where
        Q: GraphQLQuery,
        Q::Variables: Debug,
        for<'d> Q::ResponseData: Deserialize<'d>,
    {
        info!(
            target: "github",
            "sending GraphQL query '{}' {:?}",
            query.operation_name,
            query.variables,
        );
        let rsp = self
            .client
            .post(self.gql_endpoint.clone())
            .headers(self.auth_header()?)
            .header(header::USER_AGENT, USER_AGENT)
            .json(query)
            .send()
            .map_err(|err| GithubError::send_request(self.gql_endpoint.clone(), err))?;
        if rsp.status().is_server_error() {
            warn!(
                target: "github",
                "service error {} for query; retrying with backoff",
                rsp.status().as_u16(),
            );
            return Err(GithubError::github_service(rsp.status()));
        }
        if !rsp.status().is_success() {
            let err = rsp
                .text()
                .unwrap_or_else(|text_err| format!("failed to extract error body: {:?}", text_err));
            return Err(GithubError::github(err));
        }

        let rsp: Response<Q::ResponseData> = rsp.json().map_err(GithubError::json_response)?;
        if let Some(errs) = rsp.errors {
            return Err(GithubError::graphql(errs));
        }
        rsp.data.ok_or_else(GithubError::no_response)
    }

    /// Send a GraphQL query.
    pub fn send<Q>(&self, query: &QueryBody<Q::Variables>) -> GithubResult<Q::ResponseData>
    where
        Q: GraphQLQuery,
        Q::Variables: Debug,
        for<'d> Q::ResponseData: Deserialize<'d>,
    {
        retry_with_backoff(|| self.send_impl::<Q>(query))
    }
}

fn retry_with_backoff<F, K>(mut go: F) -> GithubResult<K>
where
    F: FnMut() -> GithubResult<K>,
{
    let mut timeout = BACKOFF_START;
    for _ in 0..BACKOFF_LIMIT {
        match go() {
            Ok(r) => return Ok(r),
            Err(err) => {
                if err.should_backoff() {
                    thread::sleep(timeout);
                    timeout *= BACKOFF_SCALE;
                } else {
                    return Err(err);
                }
            },
        }
    }

    Err(GithubError::github_backoff())
}
