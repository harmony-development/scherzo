use super::*;

use crate::{
    api::{
        chat::{
            action::{
                dropdown::Entry as DropdownEntry, Button as ButtonAction,
                Dropdown as DropdownAction, Input as InputAction, Kind as ActionKind,
            },
            attachment::Info,
            content::{Extra, InviteAccepted, InviteRejected, RoomUpgradedToGuild},
            embed::{embed_field::Image, EmbedField, EmbedHeading},
            format::{Format as NewFormatData, *},
            overrides::Reason as NewReason,
            Action, Attachment, Content as NewContent, Embed, Format as NewFormat, FormattedText,
            ImageInfo, Message as NewMessage, Minithumbnail, Overrides as NewOverrides,
            Reaction as NewReaction,
        },
        emote::Emote as NewEmote,
        harmonytypes::{Anything as NewAnything, Empty as NewEmpty, Metadata as NewMetadata},
        profile::Profile as NewProfile,
    },
    db::profile::USER_PREFIX,
};

use chat::Message as OldMessage;
use profile::Profile as OldProfile;

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
                chat::content::Content::EmbedMessage(old) => {
                    content.embeds = old.embeds.into_iter().map(Into::into).collect();
                }
                chat::content::Content::AttachmentMessage(old) => {
                    content.attachments = old.files.into_iter().map(Into::into).collect();
                }
                chat::content::Content::PhotoMessage(old) => {
                    content.attachments = old.photos.into_iter().map(Into::into).collect();
                }
                chat::content::Content::InviteRejected(old) => {
                    content.extra = Some(Extra::InviteRejected(old.into()));
                }
                chat::content::Content::InviteAccepted(old) => {
                    content.extra = Some(Extra::InviteAccepted(old.into()));
                }
                chat::content::Content::RoomUpgradedToGuild(old) => {
                    content.extra = Some(Extra::RoomUpgradedToGuild(old.into()));
                }
            }
            content
        }),
        created_at: old.created_at,
        edited_at: old.edited_at,
        in_reply_to: old.in_reply_to,
        metadata: old.metadata.map(Into::into),
        overrides: old.overrides.map(Into::into),
        reactions: old.reactions.into_iter().map(Into::into).collect(),
    })
    .await?;
    let profile_tree = db.open_tree(b"profile").await?;
    migrate_type::<OldProfile, NewProfile, _>(&profile_tree, USER_PREFIX, |old| NewProfile {
        user_avatar: old.user_avatar,
        account_kind: old.account_kind,
        user_name: old.user_name,
        user_status: old.user_status,
    })
    .await?;
    Ok(())
});

scherzo_derive::define_proto_mod!(before_proto_v2, harmonytypes, chat, emote, profile);

impl From<chat::Embed> for Embed {
    fn from(embed: chat::Embed) -> Self {
        Embed {
            header: embed.header.map(Into::into),
            title: embed.title,
            body: embed.body.map(Into::into),
            fields: embed.fields.into_iter().map(Into::into).collect(),
            footer: embed.footer.map(Into::into),
            color: embed.color,
        }
    }
}

impl From<chat::embed::EmbedHeading> for EmbedHeading {
    fn from(old: chat::embed::EmbedHeading) -> Self {
        EmbedHeading {
            url: old.url,
            icon: old.icon,
            subtext: old.subtext,
            text: old.text,
        }
    }
}

impl From<chat::embed::EmbedField> for EmbedField {
    fn from(old: chat::embed::EmbedField) -> Self {
        EmbedField {
            presentation: old.presentation,
            title: old.title,
            subtitle: old.subtitle,
            body: old.body.map(Into::into),
            image: old.image_url.map(|id| Image {
                id,
                ..Default::default()
            }),
            actions: old.actions.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<chat::Action> for Action {
    fn from(old: chat::Action) -> Self {
        Action {
            action_type: old.action_type,
            kind: old.kind.map(Into::into),
        }
    }
}

impl From<chat::action::Kind> for ActionKind {
    fn from(old: chat::action::Kind) -> Self {
        match old {
            chat::action::Kind::Button(old) => ActionKind::Button(ButtonAction {
                data: old.data,
                text: old.text,
                url: old.url,
            }),
            chat::action::Kind::Dropdown(old) => ActionKind::Dropdown(DropdownAction {
                entries: old.entries.into_iter().map(Into::into).collect(),
                label: old.label,
            }),
            chat::action::Kind::Input(old) => ActionKind::Input(InputAction {
                data: old.data,
                label: old.label,
                multiline: old.multiline,
            }),
        }
    }
}

impl From<chat::action::dropdown::Entry> for DropdownEntry {
    fn from(old: chat::action::dropdown::Entry) -> Self {
        DropdownEntry {
            data: old.data,
            label: old.label,
        }
    }
}

impl From<chat::content::RoomUpgradedToGuild> for RoomUpgradedToGuild {
    fn from(old: chat::content::RoomUpgradedToGuild) -> Self {
        RoomUpgradedToGuild {
            upgraded_by: old.upgraded_by,
        }
    }
}

impl From<chat::content::InviteAccepted> for InviteAccepted {
    fn from(old: chat::content::InviteAccepted) -> Self {
        InviteAccepted {
            inviter_id: old.inviter_id,
            invitee_id: old.invitee_id,
        }
    }
}

impl From<chat::content::InviteRejected> for InviteRejected {
    fn from(old: chat::content::InviteRejected) -> Self {
        InviteRejected {
            inviter_id: old.inviter_id,
            invitee_id: old.invitee_id,
        }
    }
}

impl From<chat::Photo> for Attachment {
    fn from(old: chat::Photo) -> Self {
        Attachment {
            id: old.hmc,
            name: old.name,
            // TODO: read the image for the actual mimetype
            mimetype: "image/webp".to_string(),
            size: old.file_size,
            info: Some(Info::Image(ImageInfo {
                caption: old.caption.map(|f| f.text),
                height: old.height,
                width: old.width,
                minithumbnail: old.minithumbnail.map(Into::into),
            })),
        }
    }
}

impl From<chat::Minithumbnail> for Minithumbnail {
    fn from(old: chat::Minithumbnail) -> Self {
        Minithumbnail {
            height: old.height,
            width: old.width,
            data: old.data,
        }
    }
}

impl From<chat::Attachment> for Attachment {
    fn from(old: chat::Attachment) -> Self {
        Attachment {
            id: old.id,
            name: old.name,
            mimetype: old.mimetype,
            size: old.size,
            info: None,
        }
    }
}

impl From<harmonytypes::Metadata> for NewMetadata {
    fn from(old: harmonytypes::Metadata) -> Self {
        NewMetadata {
            kind: old.kind,
            extension: old
                .extension
                .into_iter()
                .map(|(key, val)| (key, NewAnything::from(val)))
                .collect(),
        }
    }
}

impl From<harmonytypes::Anything> for NewAnything {
    fn from(old: harmonytypes::Anything) -> Self {
        NewAnything {
            body: old.body,
            kind: old.kind,
        }
    }
}

impl From<emote::Emote> for NewEmote {
    fn from(old: emote::Emote) -> Self {
        NewEmote {
            name: old.name,
            image_id: old.image_id,
        }
    }
}

impl From<chat::Reaction> for NewReaction {
    fn from(old: chat::Reaction) -> Self {
        NewReaction {
            count: old.count,
            emote: old.emote.map(Into::into),
        }
    }
}

impl From<chat::Overrides> for NewOverrides {
    fn from(old: chat::Overrides) -> Self {
        Self {
            avatar: old.avatar,
            username: old.username,
            reason: old.reason.map(Into::into),
        }
    }
}

impl From<chat::overrides::Reason> for NewReason {
    fn from(old: chat::overrides::Reason) -> Self {
        match old {
            chat::overrides::Reason::UserDefined(e) => NewReason::UserDefined(e),
            chat::overrides::Reason::Webhook(e) => NewReason::Webhook(e.into()),
            chat::overrides::Reason::SystemPlurality(e) => NewReason::SystemPlurality(e.into()),
            chat::overrides::Reason::SystemMessage(e) => NewReason::SystemMessage(e.into()),
            chat::overrides::Reason::Bridge(e) => NewReason::Bridge(e.into()),
        }
    }
}

impl From<chat::FormattedText> for FormattedText {
    fn from(old: chat::FormattedText) -> Self {
        FormattedText {
            text: old.text,
            format: old.format.into_iter().map(Into::into).collect(),
        }
    }
}

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
            format: old.format.map(Into::into),
        }
    }
}

impl From<chat::format::Format> for NewFormatData {
    fn from(old: chat::format::Format) -> Self {
        match old {
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
            chat::format::Format::GuildMention(old) => NewFormatData::GuildMention(GuildMention {
                guild_id: old.guild_id,
                homeserver: old.homeserver,
            }),
            chat::format::Format::Emoji(old) => NewFormatData::Emoji(Emoji {
                emote: Some(NewEmote {
                    image_id: old.image_hmc,
                    // TODO: actually get the emote name using the pack_id from old
                    name: String::new(),
                }),
            }),
            chat::format::Format::Color(old) => NewFormatData::Color(Color { kind: old.kind }),
            chat::format::Format::Localization(old) => NewFormatData::Localization(Localization {
                i18n_code: old.i18n_code,
            }),
        }
    }
}
