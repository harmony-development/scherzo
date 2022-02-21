use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::quote;
use syn::{parse_macro_input, AttributeArgs, ItemFn};

#[proc_macro]
pub fn define_proto_mod(input: TokenStream) -> TokenStream {
    let input = input.to_string();
    let mut split = input.split(',').map(str::trim).collect::<Vec<_>>();
    let (proto_name, svcs) = if split.len() == 1 {
        ("main", split)
    } else {
        let proto_name = split.remove(0);
        (proto_name, split)
    };

    let mut gen = TokenStream::new();
    for svc in svcs {
        let path = format!("/{}/protocol.{}.v1.rs", proto_name, svc);
        let svc = Ident::new(svc, Span::call_site());

        gen.extend(TokenStream::from(quote! {
            pub mod #svc {
                pub mod v1 {
                    include!(concat!(env!("OUT_DIR"), #path));
                }
                pub use v1::*;
            }
        }));
    }

    gen
}

#[proc_macro]
pub fn impl_db_methods(input: TokenStream) -> TokenStream {
    let input = proc_macro2::TokenStream::from(input);
    (quote! {
        pub async fn apply_batch(&self, batch: Batch) -> Result<(), ServerError> {
            self. #input .apply_batch(batch).await.map_err(ServerError::DbError)
        }

        pub async fn insert(&self, key: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> Result<Option<EVec>, ServerError> {
            self. #input .insert(key.as_ref(), value.as_ref()).await.map_err(ServerError::DbError)
        }

        pub async fn remove(&self, key: impl AsRef<[u8]>) -> Result<Option<EVec>, ServerError> {
            self. #input .remove(key.as_ref()).await.map_err(ServerError::DbError)
        }

        pub async fn get(&self, key: impl AsRef<[u8]>) -> Result<Option<EVec>, ServerError> {
            self. #input .get(key.as_ref()).await.map_err(ServerError::DbError)
        }

        pub async fn contains_key(&self, key: impl AsRef<[u8]>) -> Result<bool, ServerError> {
            self. #input .contains_key(key.as_ref()).await.map_err(ServerError::DbError)
        }

        pub async fn scan_prefix<'a>(&'a self, prefix: impl AsRef<[u8]>) -> impl Iterator<Item = Result<(EVec, EVec), ServerError>> + 'a {
            self. #input .scan_prefix(prefix.as_ref()).await.map(|res| res.map_err(ServerError::DbError))
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
        fn #func_name (&self) -> Option<hrpc::server::HrpcLayer> {
            use hrpc::server::HrpcLayer;

            (!self.disable_ratelimits)
                .then(|| HrpcLayer::new(crate::utils::rate_limit(
                    #num,
                    std::time::Duration::from_secs(#dur),
                    self.deps.config.policy.ratelimit.client_ip_header_name.clone(),
                    self.deps.config.policy.ratelimit.allowed_ips.clone(),
                )))
        }

        #func
    })
    .into()
}
