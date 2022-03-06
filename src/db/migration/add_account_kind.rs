use super::*;

use db::profile::USER_PREFIX;
use harmony_rust_sdk::api::profile::{user_status, AccountKind, UserStatus};

use crate::api::profile::Profile as NewProfile;
use profile::{Profile as OldProfile, UserStatus as OldUserStatus};

define_migration!(|db| {
    let profile_tree = db.open_tree(b"profile").await?;
    migrate_type::<OldProfile, NewProfile, _>(&profile_tree, USER_PREFIX, |old_profile| {
        NewProfile {
            user_avatar: old_profile.user_avatar,
            account_kind: AccountKind::FullUnspecified.into(),
            user_name: old_profile.user_name,
            user_status: Some(UserStatus {
                kind: (match OldUserStatus::from_i32(old_profile.user_status).unwrap_or_default() {
                    OldUserStatus::Online => user_status::Kind::Online,
                    OldUserStatus::Idle => user_status::Kind::Idle,
                    OldUserStatus::DoNotDisturb => user_status::Kind::DoNotDisturb,
                    _ => user_status::Kind::OfflineUnspecified,
                })
                .into(),
                ..Default::default()
            }),
        }
    })
    .await
});

scherzo_derive::define_proto_mod!(before_account_kind, harmonytypes, profile);
