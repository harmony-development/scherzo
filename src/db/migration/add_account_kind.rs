use super::*;

use db::profile::USER_PREFIX;
use harmony_rust_sdk::api::profile::AccountKind;

use crate::api::profile::Profile as NewProfile;
use profile::Profile as OldProfile;

define_migration!(|db| {
    let profile_tree = db.open_tree(b"profile").await?;
    migrate_type::<OldProfile, NewProfile, _>(&profile_tree, USER_PREFIX, |old_profile| {
        NewProfile {
            user_avatar: old_profile.user_avatar,
            account_kind: AccountKind::FullUnspecified.into(),
            user_name: old_profile.user_name,
            user_status: old_profile.user_status,
        }
    })
    .await
});

scherzo_derive::define_proto_mod!(before_account_kind, profile);
scherzo_derive::define_proto_mod!(before_account_kind, harmonytypes);
