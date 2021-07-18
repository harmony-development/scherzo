use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, AttributeArgs, ItemFn};

#[proc_macro]
pub fn auth(_: TokenStream) -> TokenStream {
    (quote! {
        #[allow(unused_variable)]
        let user_id = crate::impls::auth::check_auth(&self.valid_sessions, &request)?;
    })
    .into()
}

// TODO: move this to hrpc, add error reporting for invalid inputs
/// Apply a rate limit to this endpoint.
#[proc_macro_attribute]
pub fn rate(args: TokenStream, input: TokenStream) -> TokenStream {
    let mut args = parse_macro_input!(args as AttributeArgs);
    let func = parse_macro_input!(input as ItemFn);

    let dur = args.pop().unwrap();
    let num = args.pop().unwrap();

    let func_name = quote::format_ident!("{}_pre", func.sig.ident);

    (quote! {
        fn #func_name (&self) -> harmony_rust_sdk::api::exports::hrpc::warp::filters::BoxedFilter<()> {
            use harmony_rust_sdk::api::exports::hrpc::warp::Filter;

            crate::DISABLE_RATELIMITS.load(std::sync::atomic::Ordering::Relaxed)
                .then(|| harmony_rust_sdk::api::exports::hrpc::warp::any().boxed())
                .unwrap_or_else(|| crate::impls::rate(#num, #dur))
        }

        #func
    })
    .into()
}
