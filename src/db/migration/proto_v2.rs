use super::*;

use crate::api::{
    chat::{
        attachment::Info,
        content::{Extra, InviteAccepted, InviteRejected, RoomUpgradedToGuild},
        format::{Format as NewFormatData, *},
        overrides::Reason as NewReason,
        Attachment, Content as NewContent, Format as NewFormat, ImageInfo, Message as NewMessage,
        Minithumbnail, Overrides as NewOverrides, Reaction as NewReaction,
    },
    emote::Emote as NewEmote,
    harmonytypes::{Anything as NewAnything, Empty as NewEmpty, Metadata as NewMetadata},
};
use chat::Message as OldMessage;

define_migration!(|db| {
    let chat_tree = db.open_tree(b"chat").await?;
    migrate_type::<OldMessage, NewMessage, _>(&chat_tree, &[], |old| NewMessage {
        author_id: old.author_id,
        content: old.content.and_then(|c| c.content).map(|old| {
            let mut content = NewContent::default();
            match old {
                chat::content::Content::TextMessage(text) => {
                    if let Some(text) = text.content {
                        content = content.with_text(text.text).with_text_formats(
                            text.format.into_iter().map(From::from).collect::<Vec<_>>(),
                        );
                    }
                }
                // TODO: I HATE ONEOFS I HATE ONEOFS
                chat::content::Content::EmbedMessage(_) => todo!(),
                chat::content::Content::AttachmentMessage(old) => {
                    content.attachments = old
                        .files
                        .into_iter()
                        .map(|file| Attachment {
                            id: file.id,
                            name: file.name,
                            mimetype: file.mimetype,
                            size: file.size,
                            info: None,
                        })
                        .collect();
                }
                chat::content::Content::PhotoMessage(old) => {
                    content.attachments = old
                        .photos
                        .into_iter()
                        .map(|photo| Attachment {
                            id: photo.hmc,
                            name: photo.name,
                            // TODO: read the image for the actual mimetype
                            mimetype: "image/webp".to_string(),
                            size: photo.file_size,
                            info: Some(Info::Image(ImageInfo {
                                caption: photo.caption.map(|f| f.text),
                                height: photo.height,
                                width: photo.width,
                                minithumbnail: photo.minithumbnail.map(|old| Minithumbnail {
                                    height: old.height,
                                    width: old.width,
                                    data: old.data,
                                }),
                            })),
                        })
                        .collect();
                }
                chat::content::Content::InviteRejected(old) => {
                    content.extra = Some(Extra::InviteRejected(InviteRejected {
                        inviter_id: old.inviter_id,
                        invitee_id: old.invitee_id,
                    }));
                }
                chat::content::Content::InviteAccepted(old) => {
                    content.extra = Some(Extra::InviteAccepted(InviteAccepted {
                        inviter_id: old.inviter_id,
                        invitee_id: old.invitee_id,
                    }));
                }
                chat::content::Content::RoomUpgradedToGuild(old) => {
                    content.extra = Some(Extra::RoomUpgradedToGuild(RoomUpgradedToGuild {
                        upgraded_by: old.upgraded_by,
                    }));
                }
            }
            content
        }),
        created_at: old.created_at,
        edited_at: old.edited_at,
        in_reply_to: old.in_reply_to,
        metadata: old.metadata.map(|old| NewMetadata {
            kind: old.kind,
            extension: old
                .extension
                .into_iter()
                .map(|(key, val)| {
                    (
                        key,
                        NewAnything {
                            body: val.body,
                            kind: val.kind,
                        },
                    )
                })
                .collect(),
        }),
        overrides: old.overrides.map(|old| NewOverrides {
            avatar: old.avatar,
            username: old.username,
            reason: old.reason.map(|old| match old {
                chat::overrides::Reason::UserDefined(e) => NewReason::UserDefined(e),
                chat::overrides::Reason::Webhook(e) => NewReason::Webhook(e.into()),
                chat::overrides::Reason::SystemPlurality(e) => NewReason::SystemPlurality(e.into()),
                chat::overrides::Reason::SystemMessage(e) => NewReason::SystemMessage(e.into()),
                chat::overrides::Reason::Bridge(e) => NewReason::Bridge(e.into()),
            }),
        }),
        reactions: old
            .reactions
            .into_iter()
            .map(|old| NewReaction {
                count: old.count,
                emote: old.emote.map(|old| NewEmote {
                    name: old.name,
                    image_id: old.image_id,
                }),
            })
            .collect(),
    })
    .await?;
    Ok(())
});

scherzo_derive::define_proto_mod!(before_proto_v2, harmonytypes, chat, emote, profile);

impl From<harmonytypes::Empty> for NewEmpty {
    fn from(_: harmonytypes::Empty) -> Self {
        NewEmpty {}
    }
}

impl From<chat::Format> for NewFormat {
    fn from(old: chat::Format) -> Self {
        NewFormat {
            length: old.length,
            start: old.start,
            format: old.format.map(|old| match old {
                chat::format::Format::Bold(_) => NewFormatData::Bold(Bold {}),
                chat::format::Format::Italic(_) => NewFormatData::Italic(Italic {}),
                chat::format::Format::Underline(_) => NewFormatData::Underline(Underline {}),
                chat::format::Format::Monospace(_) => NewFormatData::Monospace(Monospace {}),
                chat::format::Format::Superscript(_) => NewFormatData::Superscript(Superscript {}),
                chat::format::Format::Subscript(_) => NewFormatData::Subscript(Subscript {}),
                chat::format::Format::CodeBlock(old) => NewFormatData::CodeBlock(CodeBlock {
                    language: old.language,
                }),
                chat::format::Format::UserMention(old) => NewFormatData::UserMention(UserMention {
                    user_id: old.user_id,
                }),
                chat::format::Format::RoleMention(old) => NewFormatData::RoleMention(RoleMention {
                    role_id: old.role_id,
                }),
                chat::format::Format::ChannelMention(old) => {
                    NewFormatData::ChannelMention(ChannelMention {
                        channel_id: old.channel_id,
                    })
                }
                chat::format::Format::GuildMention(old) => {
                    NewFormatData::GuildMention(GuildMention {
                        guild_id: old.guild_id,
                        homeserver: old.homeserver,
                    })
                }
                chat::format::Format::Emoji(old) => NewFormatData::Emoji(Emoji {
                    emote: Some(NewEmote {
                        image_id: old.image_hmc,
                        // TODO: actually get the emote name using the pack_id from old
                        name: String::new(),
                    }),
                }),
                chat::format::Format::Color(old) => NewFormatData::Color(Color { kind: old.kind }),
                chat::format::Format::Localization(old) => {
                    NewFormatData::Localization(Localization {
                        i18n_code: old.i18n_code,
                    })
                }
            }),
        }
    }
}
