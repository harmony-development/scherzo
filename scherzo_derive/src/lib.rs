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

#[proc_macro]
pub fn send_event(_: TokenStream) -> TokenStream {
    (quote! {
        if let Some(subbed_to) = subbed_to.get(&user_id) {
            for (stream_id, subbed_to) in subbed_to.iter() {
                if subbed_to.read().contains(&sub) {
                    if let Some(PermCheck {
                        guild_id,
                        channel_id,
                        check_for,
                        must_be_guild_owner,
                    }) = perm_check
                    {
                        let perm = chat_tree.check_perms(
                            guild_id,
                            channel_id,
                            user_id,
                            check_for,
                            must_be_guild_owner,
                        );
                        if !matches!(
                            perm,
                            Ok(_) | Err(ServerError::EmptyPermissionQuery)
                        ) {
                            continue;
                        }
                    }

                    if let Some(chan) = event_chans.get(stream_id) {
                        if let Err(err) = chan.send(event.clone()).await {
                            tracing::error!(
                                "failed to send event to stream {} of user {}: {}",
                                stream_id,
                                user_id,
                                err
                            );
                        }
                    }
                }
            }
        }
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
