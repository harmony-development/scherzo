use hrpc::{
    common::transport::http::{content_header_value, version_header_name, version_header_value},
    exports::{futures_util::FutureExt, http},
    server::transport::http::{box_body, HttpRequest as Request, HttpResponse as Response},
};
use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    future::Future,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    str::FromStr,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tower::Service;

/// Enforces a rate limit on the number of requests the underlying
/// service can handle over a period of time.
pub struct RateLimit<T> {
    inner: T,
    rate: Rate,
    global_state: State,
    keyed_states: HashMap<IpAddr, State>,
    check_header_for_ip: Option<String>,
    allowed_ips: Option<HashSet<IpAddr, ahash::RandomState>>,
}

#[derive(Debug)]
enum State {
    // The service has hit its limit
    Limited { after: Instant },
    Ready { until: Instant, rem: u64 },
}

impl State {
    fn new_ready(rate: &Rate) -> Self {
        State::Ready {
            rem: rate.num(),
            until: Instant::now(),
        }
    }
}

impl<S> RateLimit<S> {
    /// Create a new rate limiter
    pub fn new(
        inner: S,
        num: u64,
        per: Duration,
        check_header_for_ip: Option<String>,
        allowed_ips: Option<Vec<String>>,
    ) -> Self {
        let allowed_ips = allowed_ips.map(|ips| {
            ips.into_iter()
                .map(|s| IpAddr::from_str(&s))
                .flatten()
                .collect::<HashSet<_, ahash::RandomState>>()
        });
        let rate = Rate::new(num, per);

        RateLimit {
            inner,
            global_state: State::new_ready(&rate),
            rate,
            keyed_states: HashMap::new(),
            check_header_for_ip,
            allowed_ips,
        }
    }

    /// Get a reference to the inner service
    pub fn get_ref(&self) -> &S {
        &self.inner
    }

    /// Get a mutable reference to the inner service
    pub fn get_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    /// Consume `self`, returning the inner service
    pub fn into_inner(self) -> S {
        self.inner
    }

    fn bypass_for_key(&self, key: &IpAddr) -> bool {
        self.allowed_ips
            .as_ref()
            .map_or(false, |ips| ips.contains(key))
    }

    fn extract_key(&self, req: &mut Request) -> Option<IpAddr> {
        fn get_ip_addr(req: &Request) -> Option<IpAddr> {
            req.extensions().get::<SocketAddr>().map(|addr| addr.ip())
        }

        fn get_ip_addr_from_header(req: &Request, check_header_for_ip: &str) -> Option<IpAddr> {
            req.headers()
                .get(check_header_for_ip)
                .and_then(|val| val.to_str().ok())
                .and_then(|ips| ips.split(',').map(str::trim).next())
                .and_then(|ip_raw| IpAddr::from_str(ip_raw).ok())
        }

        self.check_header_for_ip
            .as_deref()
            .and_then(|header_name| get_ip_addr_from_header(req, header_name))
            .or_else(|| get_ip_addr(req))
    }
}

impl<S> Service<Request> for RateLimit<S>
where
    S: Service<Request, Response = Response, Error = Infallible>,
    S::Future: Unpin,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = RateLimitFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Service::poll_ready(&mut self.inner, cx)
    }

    fn call(&mut self, mut request: Request) -> Self::Future {
        let state = match self.extract_key(&mut request) {
            Some(key) => {
                if self.bypass_for_key(&key) {
                    let fut = Service::call(&mut self.inner, request);
                    return RateLimitFuture::ready(fut);
                }

                self.keyed_states
                    .entry(key)
                    .or_insert_with(|| State::new_ready(&self.rate))
            }
            None => &mut self.global_state,
        };

        match *state {
            State::Ready { mut until, mut rem } => {
                let now = Instant::now();

                // If the period has elapsed, reset it.
                if now >= until {
                    until = now + self.rate.per();
                    rem = self.rate.num();
                }

                if rem > 1 {
                    rem -= 1;
                    *state = State::Ready { until, rem };
                } else {
                    // The service is disabled until further notice
                    let after = Instant::now() + self.rate.per();
                    *state = State::Limited { after };
                }

                // Call the inner future
                let fut = Service::call(&mut self.inner, request);
                RateLimitFuture::ready(fut)
            }
            State::Limited { after } => {
                let now = Instant::now();
                if now < after {
                    tracing::trace!("rate limit exceeded.");
                    let after = after - now;
                    return RateLimitFuture::limited(after);
                }

                // Reset state
                *state = State::Ready {
                    until: now + self.rate.per(),
                    rem: self.rate.num(),
                };

                // Call the inner future
                let fut = Service::call(&mut self.inner, request);
                RateLimitFuture::ready(fut)
            }
        }
    }
}

enum RateLimitFutureInner<Fut> {
    Ready { fut: Fut },
    Limited { after: Duration },
}

/// Future for [`RateLimit`].
pub struct RateLimitFuture<Fut> {
    inner: RateLimitFutureInner<Fut>,
}

impl<Fut> RateLimitFuture<Fut> {
    fn ready(fut: Fut) -> Self {
        Self {
            inner: RateLimitFutureInner::Ready { fut },
        }
    }

    fn limited(after: Duration) -> Self {
        Self {
            inner: RateLimitFutureInner::Limited { after },
        }
    }
}

impl<Fut> Future for RateLimitFuture<Fut>
where
    Fut: Future<Output = Result<Response, Infallible>> + Unpin,
{
    type Output = Result<Response, Infallible>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.as_mut().inner {
            RateLimitFutureInner::Ready { fut } => fut.poll_unpin(cx),
            RateLimitFutureInner::Limited { after } => {
                let retry_info = hrpc::proto::RetryInfo {
                    retry_after: after.as_secs() as u32,
                };

                let err =
                    hrpc::proto::Error::new_resource_exhausted("rate limited; please try later")
                        .with_details(hrpc::encode::encode_protobuf_message(&retry_info).freeze());

                let resp = http::Response::builder()
                    .header(version_header_name(), version_header_value())
                    .header(http::header::CONTENT_TYPE, content_header_value())
                    .header(http::header::ACCEPT, content_header_value())
                    .body(box_body(hrpc::body::Body::full(
                        hrpc::encode::encode_protobuf_message(&err).freeze(),
                    )))
                    .unwrap();

                Poll::Ready(Ok(resp))
            }
        }
    }
}

#[doc(inline)]
pub use self::rate::Rate;

mod rate {
    use std::time::Duration;

    /// A rate of requests per time period.
    #[derive(Debug, Copy, Clone)]
    pub struct Rate {
        num: u64,
        per: Duration,
    }

    impl Rate {
        /// Create a new rate.
        ///
        /// # Panics
        ///
        /// This function panics if `num` or `per` is 0.
        pub fn new(num: u64, per: Duration) -> Self {
            assert!(num > 0);
            assert!(per > Duration::from_millis(0));

            Rate { num, per }
        }

        pub(crate) fn num(&self) -> u64 {
            self.num
        }

        pub(crate) fn per(&self) -> Duration {
            self.per
        }
    }
}
