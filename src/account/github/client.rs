// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::env;
use std::fmt::Debug;
use std::iter;
use std::thread;
use std::time::Duration;

use graphql_client::{GraphQLQuery, QueryBody, Response};
use itertools::Itertools as _;
use log::{info, warn};
use reqwest::blocking::Client;
use reqwest::header::{self, HeaderMap, HeaderValue};
use reqwest::{self, Url};
use serde::Deserialize;
use thiserror::Error;

/// The maximum number of times we will retry server errors.
const BACKOFF_LIMIT: usize = 5;
/// The number of seconds to start retries at.
const BACKOFF_START: Duration = Duration::from_secs(1);
/// How much to scale retry timeouts for a single query.
const BACKOFF_SCALE: u32 = 2;

/// Errors that can occur when communicating with the GitHub GraphQL API.
#[derive(Debug, Error)]
pub enum GithubError {
    /// The GraphQL endpoint URL could not be parsed.
    #[error("url parse error: {}", source)]
    UrlParse {
        #[from]
        /// The underlying URL parse error.
        source: url::ParseError,
    },
    /// An HTTP request to the GitHub API failed.
    #[error("failed to send request to {}: {}", endpoint, source)]
    SendRequest {
        /// The URL that was being contacted.
        endpoint: Url,
        /// The underlying reqwest error.
        source: reqwest::Error,
    },
    /// GitHub returned a non-success HTTP status with a body.
    #[error("github error: {}", response)]
    Github {
        /// The response body returned by GitHub.
        response: String,
    },
    /// The response body could not be deserialized as JSON.
    #[error("deserialize error: {}", source)]
    Deserialize {
        #[from]
        /// The underlying JSON deserialization error.
        source: serde_json::Error,
    },
    /// GitHub returned an HTTP server-error status code.
    #[error("github service error: {}", status)]
    GithubService {
        /// The HTTP status code returned.
        status: reqwest::StatusCode,
    },
    /// The response body could not be read as JSON via reqwest.
    #[error("json response deserialize: {}", source)]
    JsonResponse {
        /// The underlying reqwest error.
        source: reqwest::Error,
    },
    /// The GraphQL response contained one or more errors.
    #[error("graphql error: [\"{}\"]", message.iter().format("\", \""))]
    GraphQL {
        /// The list of GraphQL errors returned by the server.
        message: Vec<graphql_client::Error>,
    },
    /// The GraphQL response contained no data and no errors.
    #[error("no response from github")]
    NoResponse,
    /// All retry attempts were exhausted without a successful response.
    #[error("failure even after exponential backoff")]
    GithubBackoff,
}

impl GithubError {
    /// Returns `true` if this error should trigger an exponential-backoff retry.
    const fn should_backoff(&self) -> bool {
        matches!(self, Self::GithubService { .. })
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `SendRequest` error.
    pub const fn send_request(endpoint: Url, source: reqwest::Error) -> Self {
        Self::SendRequest {
            endpoint,
            source,
        }
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `Github` error from a raw response body string.
    pub const fn github(response: String) -> Self {
        Self::Github {
            response,
        }
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `GithubService` error.
    const fn github_service(status: reqwest::StatusCode) -> Self {
        Self::GithubService {
            status,
        }
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `JsonResponse` error.
    pub const fn json_response(source: reqwest::Error) -> Self {
        Self::JsonResponse {
            source,
        }
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `GraphQL` error from a list of GraphQL errors.
    const fn graphql(message: Vec<graphql_client::Error>) -> Self {
        Self::GraphQL {
            message,
        }
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `NoResponse` error.
    const fn no_response() -> Self {
        Self::NoResponse {}
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `GithubBackoff` error.
    const fn github_backoff() -> Self {
        Self::GithubBackoff {}
    }
}

/// Convenience alias for `Result<T, GithubError>`.
pub type GithubResult<T> = Result<T, GithubError>;

// The user agent for all queries.
/// User-agent header value sent with every HTTP request.
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
    #[expect(clippy::single_call_fn, reason = "used from dispatching code")]
    /// Create a new [`Github`] client for the given `host` and `token`.
    pub fn new<T>(host: &str, token: T) -> GithubResult<Self>
    where
        T: Into<String>,
    {
        let gql_endpoint = Url::parse(&format!("https://{host}/graphql"))?;

        Ok(Self {
            client: Client::new(),
            gql_endpoint,
            token: token.into(),
        })
    }

    /// The authorization header for GraphQL.
    fn auth_header(&self) -> HeaderMap {
        let mut header_value: HeaderValue = format!("bearer {}", self.token)
            .parse()
            .expect("the token should create a valid header value");
        header_value.set_sensitive(true);
        iter::once((header::AUTHORIZATION, header_value)).collect()
    }

    /// Send a GraphQL query.
    fn send_impl<Q>(&self, query: &QueryBody<Q::Variables>) -> GithubResult<Q::ResponseData>
    where
        Q: GraphQLQuery,
        Q::Variables: Debug,
        for<'rsp> Q::ResponseData: Deserialize<'rsp>,
    {
        info!(
            target: "github",
            "sending GraphQL query '{}' {:?}",
            query.operation_name,
            query.variables,
        );
        let http_rsp = self
            .client
            .post(self.gql_endpoint.clone())
            .headers(self.auth_header())
            .header(header::USER_AGENT, USER_AGENT)
            .json(query)
            .send()
            .map_err(|err| GithubError::send_request(self.gql_endpoint.clone(), err))?;
        if http_rsp.status().is_server_error() {
            warn!(
                target: "github",
                "service error {} for query; retrying with backoff",
                http_rsp.status().as_u16(),
            );
            return Err(GithubError::github_service(http_rsp.status()));
        }
        if !http_rsp.status().is_success() {
            let err = http_rsp
                .text()
                .unwrap_or_else(|text_err| format!("failed to extract error body: {text_err:?}"));
            return Err(GithubError::github(err));
        }

        let rsp: Response<Q::ResponseData> = http_rsp.json().map_err(GithubError::json_response)?;
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
        for<'rsp> Q::ResponseData: Deserialize<'rsp>,
    {
        retry_with_backoff(|| self.send_impl::<Q>(query))
    }
}

/// Retry `go` up to `BACKOFF_LIMIT` times with exponential backoff on service errors.
#[expect(
    clippy::single_call_fn,
    reason = "separate from generic constraints and syntax"
)]
fn retry_with_backoff<F, K>(mut go: F) -> GithubResult<K>
where
    F: FnMut() -> GithubResult<K>,
{
    let mut timeout = BACKOFF_START;
    for _ in 0..BACKOFF_LIMIT {
        match go() {
            Ok(res) => return Ok(res),
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
