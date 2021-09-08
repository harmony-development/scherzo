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

fn tree_insert(input: TokenStream, chat: impl quote::ToTokens) -> TokenStream {
    let input = input.to_string();
    let mut split = input.split('/');
    let key: proc_macro2::TokenStream = split.next().unwrap().parse().unwrap();
    let value: proc_macro2::TokenStream = split.next().unwrap().parse().unwrap();
    (quote! {
        self. #chat .insert(#key .as_ref(), #value .as_ref()).unwrap();
    })
    .into()
}

fn tree_remove(input: TokenStream, chat: impl quote::ToTokens) -> TokenStream {
    let input: proc_macro2::TokenStream = input.into();
    (quote! {
        self. #chat .remove(#input .as_ref()).unwrap()
    })
    .into()
}

fn tree_get(input: TokenStream, chat: impl quote::ToTokens) -> TokenStream {
    let input: proc_macro2::TokenStream = input.into();
    (quote! {
        self. #chat .get(#input .as_ref()).unwrap()
    })
    .into()
}

#[proc_macro]
pub fn emote_insert(input: TokenStream) -> TokenStream {
    tree_insert(input, quote! { emote_tree.inner })
}

#[proc_macro]
pub fn emote_remove(input: TokenStream) -> TokenStream {
    tree_remove(input, quote! { emote_tree.inner })
}

#[proc_macro]
pub fn emote_get(input: TokenStream) -> TokenStream {
    tree_get(input, quote! { emote_tree.inner })
}

#[proc_macro]
pub fn eemote_insert(input: TokenStream) -> TokenStream {
    tree_insert(input, quote! { inner })
}

#[proc_macro]
pub fn eemote_remove(input: TokenStream) -> TokenStream {
    tree_remove(input, quote! { inner })
}

#[proc_macro]
pub fn eemote_get(input: TokenStream) -> TokenStream {
    tree_get(input, quote! { inner })
}

#[proc_macro]
pub fn profile_insert(input: TokenStream) -> TokenStream {
    tree_insert(input, quote! { profile_tree.inner })
}

#[proc_macro]
pub fn profile_remove(input: TokenStream) -> TokenStream {
    tree_remove(input, quote! { profile_tree.inner })
}

#[proc_macro]
pub fn profile_get(input: TokenStream) -> TokenStream {
    tree_get(input, quote! { profile_tree.inner })
}

#[proc_macro]
pub fn pprofile_insert(input: TokenStream) -> TokenStream {
    tree_insert(input, quote! { inner })
}

#[proc_macro]
pub fn pprofile_remove(input: TokenStream) -> TokenStream {
    tree_remove(input, quote! { inner })
}

#[proc_macro]
pub fn pprofile_get(input: TokenStream) -> TokenStream {
    tree_get(input, quote! { inner })
}

#[proc_macro]
pub fn chat_insert(input: TokenStream) -> TokenStream {
    tree_insert(input, quote! { chat_tree.chat_tree })
}

#[proc_macro]
pub fn chat_remove(input: TokenStream) -> TokenStream {
    tree_remove(input, quote! { chat_tree.chat_tree })
}

#[proc_macro]
pub fn chat_get(input: TokenStream) -> TokenStream {
    tree_get(input, quote! { chat_tree.chat_tree })
}

#[proc_macro]
pub fn auth_insert(input: TokenStream) -> TokenStream {
    tree_insert(input, quote! { auth_tree })
}

#[proc_macro]
pub fn auth_remove(input: TokenStream) -> TokenStream {
    tree_remove(input, quote! { auth_tree })
}

#[proc_macro]
pub fn auth_get(input: TokenStream) -> TokenStream {
    tree_get(input, quote! { auth_tree })
}

#[proc_macro]
pub fn cchat_insert(input: TokenStream) -> TokenStream {
    tree_insert(input, quote! { chat_tree })
}

#[proc_macro]
pub fn cchat_remove(input: TokenStream) -> TokenStream {
    tree_remove(input, quote! { chat_tree })
}

#[proc_macro]
pub fn cchat_get(input: TokenStream) -> TokenStream {
    tree_get(input, quote! { chat_tree })
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
        fn #func_name (&self, _: &'static str) -> harmony_rust_sdk::api::exports::hrpc::warp::filters::BoxedFilter<()> {
            use harmony_rust_sdk::api::exports::hrpc::warp::Filter;

            self.disable_ratelimits
                .then(|| harmony_rust_sdk::api::exports::hrpc::warp::any().boxed())
                .unwrap_or_else(|| crate::impls::rate(#num, #dur))
        }

        #func
    })
    .into()
}
