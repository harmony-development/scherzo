use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, AttributeArgs, ItemFn};

#[proc_macro]
pub fn impl_db_methods(input: TokenStream) -> TokenStream {
    let input = proc_macro2::TokenStream::from(input);
    (quote! {
        pub fn insert(&self, key: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> ServerResult<Option<Vec<u8>>> {
            self. #input .insert(key.as_ref(), value.as_ref()).map_err(|err| ServerError::DbError(err).into())
        }

        pub fn remove(&self, key: impl AsRef<[u8]>) -> ServerResult<Option<Vec<u8>>> {
            self. #input .remove(key.as_ref()).map_err(|err| ServerError::DbError(err).into())
        }

        pub fn get(&self, key: impl AsRef<[u8]>) -> ServerResult<Option<Vec<u8>>> {
            self. #input .get(key.as_ref()).map_err(|err| ServerError::DbError(err).into())
        }

        pub fn contains_key(&self, key: impl AsRef<[u8]>) -> ServerResult<bool> {
            self. #input .contains_key(key.as_ref()).map_err(|err| ServerError::DbError(err).into())
        }

        pub fn scan_prefix<'a>(&'a self, prefix: impl AsRef<[u8]>) -> Box<dyn Iterator<Item = ServerResult<(Vec<u8>, Vec<u8>)>> + Send + 'a> {
            Box::new(self. #input .scan_prefix(prefix.as_ref()).map(|res| ServerResult::Ok(res.map_err(ServerError::DbError)?)))
        }
    }).into()
}

// TODO: move this to hrpc, add error reporting for invalid inputs
/// Apply a rate limit to this endpoint.
#[proc_macro_attribute]
pub fn rate(args: TokenStream, input: TokenStream) -> TokenStream {
    let mut args = parse_macro_input!(args as AttributeArgs);
    let func = parse_macro_input!(input as ItemFn);

    let dur = args.pop().unwrap();
    let num = args.pop().unwrap();

    let func_name = quote::format_ident!("{}_middleware", func.sig.ident);

    (quote! {
        fn #func_name (&self, _: &'static str) -> harmony_rust_sdk::api::exports::hrpc::server::HrpcLayer {
            use harmony_rust_sdk::api::exports::hrpc::server::HrpcLayer;

            self.disable_ratelimits
                .then(|| HrpcLayer::new(tower::layer::util::Identity::new()))
                .unwrap_or_else(|| HrpcLayer::new(tower::limit::RateLimitLayer::new(#num, std::time::Duration::from_secs(#dur))))
        }

        #func
    })
    .into()
}
