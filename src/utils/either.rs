use hrpc::exports::futures_util::ready;
use pin_project::pin_project;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::{layer::util::Identity, Layer, Service};

/// Combine two different service types into a single type.
///
/// Both services must be of the same request, response, and error types.
/// [`Either`] is useful for handling conditional branching in service middleware
/// to different inner service types.
#[pin_project(project = EitherProj)]
#[derive(Clone, Debug)]
pub enum Either<A, B> {
    /// One type of backing [`Service`].
    A(#[pin] A),
    /// The other type of backing [`Service`].
    B(#[pin] B),
}

impl<A, B, Request> Service<Request> for Either<A, B>
where
    A: Service<Request>,
    B: Service<Request, Response = A::Response, Error = A::Error>,
{
    type Response = A::Response;
    type Error = A::Error;
    type Future = Either<A::Future, B::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        use self::Either::*;

        match self {
            A(service) => Poll::Ready(Ok(ready!(service.poll_ready(cx))?)),
            B(service) => Poll::Ready(Ok(ready!(service.poll_ready(cx))?)),
        }
    }

    fn call(&mut self, request: Request) -> Self::Future {
        use self::Either::*;

        match self {
            A(service) => A(service.call(request)),
            B(service) => B(service.call(request)),
        }
    }
}

impl<A, B, T, Err> Future for Either<A, B>
where
    A: Future<Output = Result<T, Err>>,
    B: Future<Output = Result<T, Err>>,
{
    type Output = Result<T, Err>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project() {
            EitherProj::A(fut) => Poll::Ready(Ok(ready!(fut.poll(cx)).map_err(Into::into)?)),
            EitherProj::B(fut) => Poll::Ready(Ok(ready!(fut.poll(cx)).map_err(Into::into)?)),
        }
    }
}

impl<S, A, B> Layer<S> for Either<A, B>
where
    A: Layer<S>,
    B: Layer<S>,
{
    type Service = Either<A::Service, B::Service>;

    fn layer(&self, inner: S) -> Self::Service {
        match self {
            Either::A(layer) => Either::A(layer.layer(inner)),
            Either::B(layer) => Either::B(layer.layer(inner)),
        }
    }
}

pub fn option_layer<L>(layer: Option<L>) -> Either<L, Identity> {
    if let Some(layer) = layer {
        Either::A(layer)
    } else {
        Either::B(Identity::new())
    }
}
